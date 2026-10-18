//! Managed Codex native setup. Product instructions, MCP role and credentials are prepared by
//! the operation; native vocabulary and dispatch stay in this server Codex integration.
use std::path::PathBuf;

use super::PlannerFactory;
use crate::error::Result;
use crate::model::CardRole;
use crate::shared_codex_appserver::{SharedThreadStartParams, ThreadConfig};

impl PlannerFactory<'_> {
    /// Called only at the existing native-prepare checkpoint, never by construction or open.
    /// The initial thread remains sandboxed with no approval requests; per-turn permission
    /// policy is still read and applied by the harness when a turn is issued.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_codex_thread(
        &self,
        card_id: &str,
        card_role: CardRole,
        track_id: &str,
        cwd: String,
        developer_instructions: Option<String>,
        mcp_role: Option<CardRole>,
        socket_path: PathBuf,
        raw_token: String,
        deferred: bool,
    ) -> Result<String> {
        let daemon = self.daemon()?;
        let params = SharedThreadStartParams {
            cwd,
            approval_policy: "never".into(),
            sandbox_mode: "workspace-write".into(),
            developer_instructions,
            config: match mcp_role {
                Some(role) => ThreadConfig::McpShell {
                    role,
                    socket_path,
                    raw_token,
                },
                None => ThreadConfig::NoMcp,
            },
        };
        if deferred {
            daemon.thread_start_mint_for_card(card_id, params).await
        } else {
            daemon
                .thread_start_for_card(card_id, card_role, Some(track_id), params)
                .await
        }
    }
}
