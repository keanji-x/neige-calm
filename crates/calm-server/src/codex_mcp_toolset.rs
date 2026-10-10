//! Keeps live Codex threads' kernel tool list current when the running plugin set changes (#2014).
//!
//! Codex lists an MCP server's tools once per thread, when the thread starts or resumes, and keeps
//! that list. A plugin that started running after a thread listed never reaches it. Codex 0.159.2
//! restarts a loaded thread's MCP server, and so re-lists, on `config/mcpServer/reload` only when
//! that server's config entry changed; a reload over an unchanged entry does nothing.
//!
//! The kernel cannot know what each loaded thread cached: a thread may have listed in the middle of
//! a plugin restart, or before a calm-server restart whose events were never processed. So every
//! event that may have changed the catalog, and every daemon incarnation, bumps the kernel entry's
//! generation counter (`env.NEIGE_MCP_TOOLSET`) and then asks the daemon to reload:
//! - a debounced burst of plugin-state events, or a lagged receiver that may have missed some;
//! - every transition of the shared daemon into Running, including the boot spawn or takeover of a
//!   daemon that kept its loaded threads across a calm-server restart.
//!
//! The cost: every calm-server restart and every plugin lifecycle burst restarts each loaded
//! thread's kernel MCP shim once. A `tools/call` in flight across the restart finishes on the old
//! process, which Codex stops only after the call returns, and per-thread tool approval overrides
//! survive (both probed with a fake model). A plugin crash loop costs one restart per debounce
//! window, bounded by the host's crash window (5 crashes, backoff 1/2/4/8 s).
//!
//! A refresh whose write fails (no kernel entry, I/O), or whose reload finds no daemon connected or
//! fails, is not retried by itself: every trigger performs a whole refresh, so the next plugin
//! change or daemon Running repays it. There is no timer for a daemon that stays Running while
//! nothing changes.
//!
//! Known residual: a thread that lists during a plugin gap but is not yet loaded when the reload
//! arrives keeps its list; the debounce window makes that gap narrow.
//!
//! The same write carries the plugin approval grants (#2530): the entry's per-tool `tools` map is
//! exactly the delegated tools (manifest v7 `"approval": "delegated"`) of the installed and enabled
//! plugins, each `approval_mode = "approve"`, read from the plugin host at refresh time. Codex merges
//! it with the per-role terminal map a thread's start config carries. The kernel promises the grants
//! to threads that start after the refresh. Codex 0.159.2 also applies them to a loaded thread when
//! the reload below restarts its kernel server, since the map is part of the server entry the reload
//! re-reads, and the per-thread terminal map survives that (both probed with a fake model, #2530
//! E3c); a reload that does not reach the daemon leaves loaded threads as they were.
//!
//! Every path fails closed. A refresh whose plugin-row read fails publishes its generation with no
//! grants at all, and the next readable refresh restores them; a written generation therefore never
//! carries a grant the plugin rows did not vouch for at that refresh. Boot's rewrite of the entry
//! drops the map, so a restart never carries a grant to changed plugin code; the daemon's Running
//! refresh writes it again.
//!
//! Writes take the shared home's `.config.lock`, as every calm writer does. Codex's own config
//! writes (`config/value/write`, `config/batchWrite`, neither of which calm sends) do not.
//!
//! The task has no shutdown handle: like the other boot-time followers it lives as long as the
//! process, since the bus it follows never closes while the app context holds a sender.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::watch;

use crate::event::{BroadcastEnvelope, Event, EventBus};
use crate::plugin_host::PluginHost;
use crate::shared_codex_appserver::{DaemonReadiness, McpServerReload, SharedCodexAppServer};
use crate::shared_codex_home::SharedCodexHome;

/// How long a burst of plugin-state changes is gathered before one refresh, so a crash loop
/// cannot storm the daemon with reloads.
pub const DEBOUNCE: Duration = Duration::from_secs(1);

/// The parts a refresh writes and asks.
pub struct CodexMcpToolset {
    pub home: Arc<SharedCodexHome>,
    pub appserver: Arc<SharedCodexAppServer>,
    /// The authority on which plugins are installed and enabled, and what their manifests delegate.
    pub plugins: Arc<PluginHost>,
    pub debounce: Duration,
}

impl CodexMcpToolset {
    /// Follows plugin state and daemon readiness in a background task. Both receivers are taken
    /// here, before the daemon boots, so its first Running is seen.
    pub fn start(self, events: &EventBus) {
        let plugin_events = events.subscribe();
        let mut readiness = self.appserver.readiness_receiver();
        // A daemon already Running is handled as a transition into Running.
        readiness.mark_changed();
        // Detached: the task owns its parts.
        tokio::spawn(self.follow(plugin_events, readiness));
    }

    async fn follow(
        self,
        mut plugin_events: Receiver<BroadcastEnvelope>,
        mut readiness: watch::Receiver<DaemonReadiness>,
    ) {
        let mut seen = DaemonReadiness {
            generation: 0,
            running: false,
        };
        let mut readiness_open = true;
        loop {
            tokio::select! {
                event = plugin_events.recv() => {
                    match event {
                        Ok(envelope) if matches!(envelope.event, Event::PluginState { .. }) => {}
                        Ok(_) => continue,
                        // Missed events may include plugin-state changes.
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => return,
                    }
                    tokio::time::sleep(self.debounce).await;
                    // Drain what the window gathered; one refresh covers all of it.
                    while let Ok(_) | Err(TryRecvError::Lagged(_)) = plugin_events.try_recv() {}
                    self.refresh().await;
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
                        self.refresh().await;
                    }
                }
            }
        }
    }

    /// Bump the entry's generation and write its approval grants, then ask the daemon to reload MCP.
    /// The write comes first: a reload over an unchanged entry restarts nothing.
    async fn refresh(&self) {
        let delegated = match self
            .plugins
            .approval_delegated_tools(&crate::builtin_plugins::native_tool_names())
            .await
        {
            Ok(delegated) => delegated,
            // Fail closed: a grant the kernel cannot vouch for is revoked, not kept.
            Err(error) => {
                tracing::warn!(
                    %error,
                    "plugin rows unreadable, every approval grant revoked; the next plugin change or daemon Running restores them"
                );
                BTreeSet::new()
            }
        };
        // The write takes a file lock and fsyncs: off the async workers.
        let home = self.home.clone();
        let written = tokio::task::spawn_blocking(move || home.refresh_mcp_entry(&delegated))
            .await
            .unwrap_or_else(|join| Err(std::io::Error::other(join)));
        if let Err(error) = written {
            tracing::warn!(
                %error,
                "kernel MCP entry write failed; the next plugin change or daemon Running retries"
            );
            return;
        }
        match self.appserver.mcp_server_reload().await {
            Ok(McpServerReload::Sent) => {
                tracing::info!("asked the Codex daemon to reload MCP for the kernel catalog");
            }
            Ok(McpServerReload::NotConnected) => {
                tracing::debug!(
                    "no Codex daemon connected; its next Running bumps the generation and reloads"
                );
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "Codex MCP reload failed; the next plugin change or daemon Running reloads again"
                );
            }
        }
    }
}
