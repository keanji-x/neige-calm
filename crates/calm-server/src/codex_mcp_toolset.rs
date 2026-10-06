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
use crate::shared_codex_appserver::{DaemonReadiness, McpServerReload, SharedCodexAppServer};
use crate::shared_codex_home::SharedCodexHome;

/// How long a burst of plugin-state changes is gathered before one refresh, so a crash loop
/// cannot storm the daemon with reloads.
pub const DEBOUNCE: Duration = Duration::from_secs(1);

/// The parts a refresh writes and asks.
pub struct CodexMcpToolset {
    pub home: Arc<SharedCodexHome>,
    pub appserver: Arc<SharedCodexAppServer>,
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

    /// Bump the entry's generation, then ask the daemon to reload MCP. The write comes first: a
    /// reload over an unchanged entry restarts nothing.
    async fn refresh(&self) {
        // The bump takes a file lock and fsyncs: off the async workers.
        let home = self.home.clone();
        let bumped = tokio::task::spawn_blocking(move || home.bump_mcp_toolset())
            .await
            .unwrap_or_else(|join| Err(std::io::Error::other(join)));
        if let Err(error) = bumped {
            tracing::warn!(
                %error,
                "kernel MCP generation write failed; the next plugin change or daemon Running retries"
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
