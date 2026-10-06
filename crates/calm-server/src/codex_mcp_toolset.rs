//! Keeps live Codex threads' kernel tool list current when the running plugin set changes (#2014).
//!
//! Codex lists an MCP server's tools once per thread, when the thread starts or resumes, and keeps
//! that list. So a plugin enabled after a thread started never reaches it. On each change to plugin
//! state this task gives the kernel entry in the shared `config.toml` a new catalog generation, then
//! asks the connected daemon to reload MCP. Codex 0.159.2 then restarts the kernel MCP server of
//! every loaded thread whose entry changed, and that thread's next model request carries the new
//! list. A plain reload with an unchanged entry does nothing, which is why the generation exists.
//!
//! The generation is `<catalog digest>.<write counter>`. The digest alone misses a catalog that
//! changes and changes back inside one debounce window, for example a plugin reload or a crash and
//! respawn: a thread that started in the gap listed the catalog without that plugin, yet the final
//! digest equals the written one. So a window in which any plugin entered Running forces a write
//! with the next counter, and with it a reload. A plugin that left Running and stayed out changes
//! the digest by itself, and one that came back emitted a Running. The price: a plugin flap
//! restarts every loaded thread's kernel MCP shim once per window. Flaps are rare, either
//! initiated by the owner or a crash, and the restart is cheap next to a thread missing tools.
//! A plugin crash loop is bounded by the host's crash window (5 crashes, backoff 1/2/4/8 s): about
//! two catalog flips per cycle, so roughly ten forced reloads before the plugin goes terminal, and
//! a `tools/call` in flight survives each one. A plugin that enters Running without serving tools
//! also forces one; that keeps the rule a plain state check instead of a catalog diff per plugin.
//!
//! A written generation stays owed to the daemon until a reload is sent: when none is connected,
//! or the request fails, the reload is retried at the next daemon Running and at the next plugin
//! change. Every transition into Running also sends one reload, because an adopted daemon keeps
//! its loaded threads' MCP clients across a calm-server restart; on a daemon whose entries already
//! match, or that has nothing loaded, the reload is a no-op.
//!
//! A `tools/call` in flight across that restart finishes on the old server, which Codex stops only
//! after the call returns. This was probed with a fake model, so no gate on running turns is needed.
//!
//! Writes take the shared home's `.config.lock`, as every calm writer does. Codex's own config
//! writes (`config/value/write`, `config/batchWrite`, neither of which calm sends) do not.
//!
//! The task has no shutdown handle: like the other boot-time followers it lives as long as the
//! process, since the bus it follows never closes while the app context holds a sender.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::watch;

use crate::event::{BroadcastEnvelope, Event, EventBus};
use crate::mcp_server::transport::bootstrap_catalog_digest;
use crate::mcp_server::{AppContext, ToolRegistry};
use crate::plugin_host::PluginRuntimeStatus;
use crate::shared_codex_appserver::{DaemonReadiness, McpServerReload, SharedCodexAppServer};
use crate::shared_codex_home::SharedCodexHome;

/// `None` for an event other than plugin state; otherwise whether the plugin entered Running, the
/// one state whose tools a bootstrap `tools/list` serves.
fn entered_running(envelope: &BroadcastEnvelope) -> Option<bool> {
    match &envelope.event {
        Event::PluginState { state, .. } => Some(state == PluginRuntimeStatus::Running.wire_name()),
        _ => None,
    }
}

/// How long a burst of plugin-state changes is gathered before one refresh, so a crash loop
/// cannot storm the daemon with reloads.
pub const DEBOUNCE: Duration = Duration::from_secs(1);

/// The parts one refresh reads and writes.
pub struct CodexMcpToolset {
    /// The context the MCP listener serves `tools/list` from.
    pub ctx: Arc<AppContext>,
    /// The registry the MCP listener serves `tools/list` from.
    pub registry: Arc<ToolRegistry>,
    pub home: Arc<SharedCodexHome>,
    pub appserver: Arc<SharedCodexAppServer>,
    pub debounce: Duration,
}

impl CodexMcpToolset {
    /// Writes the current generation, then follows plugin state and daemon readiness in a
    /// background task. The boot write sends nothing itself: this process has no daemon connection
    /// yet, and the Running that the daemon's boot (spawn or takeover) publishes sends the reload.
    /// Both receivers are taken before the write, so nothing after the computed value is missed.
    pub async fn start(self, events: &EventBus) {
        let plugin_events = events.subscribe();
        let mut readiness = self.appserver.readiness_receiver();
        // A daemon already Running is handled as a transition into Running.
        readiness.mark_changed();
        // A restart over an unchanged catalog leaves the file untouched.
        self.write_generation(false).await;
        // Detached: the task owns its parts.
        tokio::spawn(self.follow(plugin_events, readiness));
    }

    async fn follow(
        self,
        mut plugin_events: Receiver<BroadcastEnvelope>,
        mut readiness: watch::Receiver<DaemonReadiness>,
    ) {
        let mut reload_owed = false;
        let mut seen = DaemonReadiness {
            generation: 0,
            running: false,
        };
        let mut readiness_open = true;
        loop {
            tokio::select! {
                event = plugin_events.recv() => {
                    let mut force = match event {
                        Ok(envelope) => match entered_running(&envelope) {
                            Some(entered) => entered,
                            None => continue,
                        },
                        // Missed events may include a plugin entering Running.
                        Err(RecvError::Lagged(_)) => true,
                        Err(RecvError::Closed) => return,
                    };
                    tokio::time::sleep(self.debounce).await;
                    // Drain what the window gathered; one refresh covers all of it.
                    loop {
                        match plugin_events.try_recv() {
                            Ok(envelope) => {
                                force |= entered_running(&envelope) == Some(true);
                            }
                            Err(TryRecvError::Lagged(_)) => force = true,
                            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                        }
                    }
                    if self.write_generation(force).await || reload_owed {
                        reload_owed = !self.reload().await;
                    }
                }
                changed = readiness.changed(), if readiness_open => {
                    if changed.is_err() {
                        readiness_open = false;
                        continue;
                    }
                    let now = *readiness.borrow_and_update();
                    let daemon_came_up =
                        now.running && !(seen.running && seen.generation == now.generation);
                    seen = now;
                    if daemon_came_up {
                        reload_owed = !self.reload().await;
                    }
                }
            }
        }
    }

    /// Whether the kernel entry was rewritten with a new generation; the caller owes the daemon a
    /// reload for it. `force` rewrites it even when the catalog digest is unchanged.
    async fn write_generation(&self, force: bool) -> bool {
        let digest = bootstrap_catalog_digest(&self.ctx, &self.registry).await;
        match self.home.ensure_mcp_toolset(&digest, force) {
            Ok(changed) => changed,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "kernel MCP catalog generation write failed; it is retried at the next plugin change"
                );
                false
            }
        }
    }

    /// Whether the reload was sent; otherwise it stays owed.
    async fn reload(&self) -> bool {
        match self.appserver.mcp_server_reload().await {
            Ok(McpServerReload::Sent) => {
                tracing::info!("asked the Codex daemon to reload MCP for the kernel catalog");
                true
            }
            Ok(McpServerReload::NotConnected) => {
                tracing::debug!(
                    "no Codex daemon connected; the MCP reload is sent at its next Running"
                );
                false
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "Codex MCP reload failed; it is retried at the next daemon Running or plugin change"
                );
                false
            }
        }
    }
}
