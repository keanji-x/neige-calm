//! Keeps live Codex threads' kernel tool list current when the running plugin set changes (#2014).
//!
//! Codex lists an MCP server's tools once per thread, when the thread starts or resumes, and keeps
//! that list. So a plugin enabled after a thread started never reaches it. On each change to plugin
//! state this task gives the kernel entry in the shared `config.toml` a new catalog generation, then
//! asks the connected daemon to reload MCP. Codex 0.159.2 then restarts the kernel MCP server of
//! every loaded thread whose entry changed, and that thread's next model request carries the new
//! list. A plain reload with an unchanged entry does nothing, which is why the generation exists.
//!
//! A `tools/call` in flight across that restart finishes on the old server, which Codex stops only
//! after the call returns. This was probed with a fake model, so no gate on running turns is needed.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use crate::event::{BroadcastEnvelope, Event, EventBus};
use crate::mcp_server::transport::bootstrap_catalog_generation;
use crate::mcp_server::{AppContext, ToolRegistry};
use crate::shared_codex_appserver::{McpServerReload, SharedCodexAppServer};
use crate::shared_codex_home::SharedCodexHome;

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
    /// Writes the current generation, then follows plugin state in a background task. The boot
    /// write sends no reload: this process has no daemon connection yet, and the daemon it starts
    /// or adopts resumes its threads from this config. The receiver is taken before the write, so
    /// a change after the computed value is not missed.
    pub async fn start(self, events: &EventBus) {
        let rx = events.subscribe();
        self.write_generation().await;
        // Detached: the task owns its parts and ends when the bus closes.
        tokio::spawn(self.follow(rx));
    }

    async fn follow(self, mut rx: Receiver<BroadcastEnvelope>) {
        loop {
            match rx.recv().await {
                Ok(envelope) if matches!(envelope.event, Event::PluginState { .. }) => {}
                Ok(_) => continue,
                // Missed events may include a plugin-state change, so refresh.
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
            tokio::time::sleep(self.debounce).await;
            // Drain what the window gathered; one refresh covers all of it.
            while let Ok(_) | Err(TryRecvError::Lagged(_)) = rx.try_recv() {}
            if self.write_generation().await {
                self.reload().await;
            }
        }
    }

    /// Whether the kernel entry was rewritten with a new generation.
    async fn write_generation(&self) -> bool {
        let generation = bootstrap_catalog_generation(&self.ctx, &self.registry).await;
        match self.home.ensure_mcp_toolset(&generation) {
            Ok(changed) => changed,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "kernel MCP catalog generation write failed; live Codex threads keep their tool list until the next plugin change"
                );
                false
            }
        }
    }

    async fn reload(&self) {
        match self.appserver.mcp_server_reload().await {
            Ok(McpServerReload::Sent) => {
                tracing::info!("kernel MCP catalog changed; asked the Codex daemon to reload MCP");
            }
            Ok(McpServerReload::NotConnected) => {
                tracing::debug!(
                    "kernel MCP catalog changed with no Codex daemon connected; the next daemon reads it"
                );
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "Codex MCP reload failed; live threads keep their tool list until the next plugin change"
                );
            }
        }
    }
}
