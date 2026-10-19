//! Managed Codex native setup. Product instructions, MCP role and credentials are prepared by
//! the operation; native vocabulary and dispatch stay in this server Codex integration.
use std::path::PathBuf;

use super::PlannerFactory;
use crate::error::Result;
use crate::model::CardRole;
use crate::shared_codex_appserver::{SharedThreadStartParams, ThreadConfig};

impl PlannerFactory<'_> {
    /// Registry-miss stop uses only the persisted runtime's binding and fallback fact.
    pub(super) async fn shutdown_codex_replay(
        &self,
        runtime: &crate::session_projection_repo::WorkerSessionProjection,
    ) -> Result<()> {
        let Some(thread_id) = runtime.thread_id.as_deref() else {
            return Ok(());
        };
        let daemon = self.daemon()?;
        super::super::interrupt_codex_thread(
            &daemon,
            thread_id,
            runtime.active_turn_id.as_deref(),
            |step, e| match step {
                super::super::CodexInterruptStep::ActiveTurn => tracing::warn!(
                    runtime_id = %runtime.id, thread_id, error = %e,
                    "planner harness shutdown replay thread interrupt failed"
                ),
                super::super::CodexInterruptStep::FallbackTurn(persisted_turn) => tracing::warn!(
                    runtime_id = %runtime.id, thread_id, turn_id = persisted_turn, error = %e,
                    "planner harness shutdown replay persisted-turn interrupt failed"
                ),
            },
        )
        .await;
        Ok(())
    }

    /// Called only at the existing native-prepare checkpoint, never by construction or open.
    /// The initial thread remains sandboxed with no approval requests; per-turn permission
    /// policy is still read and applied by the harness when a turn is issued.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_codex_thread(
        &self,
        card_id: &str,
        card_role: CardRole,
        _track_id: &str,
        cwd: String,
        developer_instructions: Option<String>,
        mcp_role: Option<CardRole>,
        socket_path: PathBuf,
        raw_token: String,
        deferred: bool,
        admission: &crate::shared_codex_appserver::NativePromptAdmission<'_>,
    ) -> std::result::Result<String, crate::shared_codex_appserver::ThreadMintFailure> {
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
            daemon
                .thread_start_mint_admitted_for_card(card_id, params, admission)
                .await
        } else {
            daemon
                .thread_start_admitted_for_card(card_id, Some(card_role), params, admission)
                .await
        }
    }
}
