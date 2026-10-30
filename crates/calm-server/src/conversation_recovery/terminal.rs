//! Current namespace, workspace and source admission around native recovery I/O.
use super::*;
use crate::harness::native_admission::NativePromptAdmission;
use crate::shared_codex_appserver::{IdleDetachGuard, SharedCodexAppServer};

pub(super) enum RecoveryProof {
    Provider(crate::provider_execution::quiescence::Quiescence),
    Codex {
        native: IdleDetachGuard,
        transcript: crate::harness::transcript_receipt::CanonicalTerminalReceipt,
        daemon: std::sync::Arc<SharedCodexAppServer>,
    },
}
impl RecoveryProof {
    pub(super) async fn recheck_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<()> {
        match self {
            Self::Provider(proof) => proof.recheck_tx(tx).await,
            Self::Codex { native, daemon, .. }
                if native.recheck() && daemon.authentication_hold().is_none() =>
            {
                Ok(())
            }
            Self::Codex { .. } => Err(CalmError::Conflict("native recovery source changed".into())),
        }
    }
}
#[derive(PartialEq)]
pub(super) struct NativeContext {
    namespace: String,
    registry: Option<(String, String)>,
    workspace: calm_truth::model::TrackWorkspace,
}
impl NativeContext {
    pub(super) async fn capture_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> Result<Self> {
        use sqlite::session_backend::{CodexScope, CodexScopeKey, codex_backend_scope_tx};
        let CodexScope::Bound(namespace) =
            codex_backend_scope_tx(tx, CodexScopeKey::Session(carrier.worker_session_id())).await?
        else {
            return Err(CalmError::Conflict("native backend unavailable".into()));
        };
        let (_, row) = crate::harness::profile::input_carrier_contract_tx(tx, carrier).await?;
        let track = sqlite::track_get_tx(tx, &row.track_id).await?;
        if !workspace_runtime::recycle::workspace_allows_runtime_recovery(&track) {
            return Err(CalmError::Conflict(
                "Restore the conversation workspace before recovery".into(),
            ));
        }
        crate::workspace_service::validate_runtime_workspace(&track.workspace)?;
        let registry = sqlx::query_as("SELECT hex(configured_home),hex(canonical_home) FROM agent_backend_registry WHERE provider='codex' AND backend_id=?1").bind(&namespace).fetch_optional(&mut **tx).await?;
        Ok(Self {
            namespace,
            registry,
            workspace: track.workspace,
        })
    }
    pub(super) async fn recheck_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        carrier: &CurrentInputCarrier,
    ) -> Result<()> {
        if Self::capture_tx(tx, carrier).await? != *self {
            return Err(CalmError::Conflict(
                "recovery backend or workspace changed".into(),
            ));
        }
        Ok(())
    }
}
pub(super) async fn prove(
    s: &RouteState,
    source: &HumanInputSource,
    captured: &CurrentInputCarrier,
    binding: &crate::harness::profile::PlannerBinding,
    context: &NativeContext,
    daemon: &std::sync::Arc<SharedCodexAppServer>,
) -> Result<RecoveryProof> {
    if daemon.backend_id().as_str() != context.namespace || daemon.authentication_hold().is_some() {
        return Err(CalmError::Conflict(
            "native backend credentials are unavailable".into(),
        ));
    }
    let db = s
        .mcp_context
        .sqlite_db
        .as_ref()
        .ok_or(CalmError::ExecutionUnavailable)?;
    let admit = NativePromptAdmission::new(
        || source.current_principal().map(drop),
        || {
            Box::pin(async {
                let mut tx = sqlite::begin_immediate_tx(db).await?;
                authorize(&mut tx, source, captured.card_id()).await?;
                let current = current_input_fact_carrier_tx(
                    &mut tx,
                    captured.card_id(),
                    captured.worker_session_id(),
                )
                .await?;
                let (current_binding, _) =
                    crate::harness::profile::input_carrier_contract_tx(&mut tx, &current).await?;
                if current.original_snapshot() != captured.original_snapshot()
                    || &current_binding != binding
                    || terminal_recovery_request_tx(&mut tx, &current)
                        .await?
                        .is_none()
                {
                    return Err(CalmError::Conflict(
                        "terminal recovery target changed".into(),
                    ));
                }
                context.recheck_tx(&mut tx, &current).await?;
                tx.rollback().await?;
                Ok(())
            })
        },
    );
    let runtime = s
        .repo
        .session_projection_by_id(captured.worker_session_id())
        .await?
        .ok_or_else(|| CalmError::Conflict("recovery carrier disappeared".into()))?;
    let target = crate::harness::backend::factory::RecoveryTarget::known_failed(&runtime)?;
    let turn = captured.snapshot()["last_turn_id"]
        .as_str()
        .ok_or_else(|| CalmError::Conflict("failed turn missing".into()))?;
    let prepared = daemon
        .terminal_recovery_guard(&target, turn, &admit)
        .await
        .map_err(provider_confirmation_error)?;
    Ok(RecoveryProof::Codex {
        native: prepared.native,
        transcript: prepared.transcript,
        daemon: daemon.clone(),
    })
}

/// Provider transport/JSON-RPC errors may contain private request echoes. The
/// canonical recovery owner preserves local authorization refusals but exposes
/// one fixed diagnostic for backend confirmation failures.
fn provider_confirmation_error(error: CalmError) -> CalmError {
    match error {
        CalmError::CodexAppServer(_) | CalmError::CodexRefused(_) | CalmError::Serde(_) =>
            CalmError::ServiceUnavailable("The provider could not confirm recovery. History and queued messages are retained.".into()),
        local => local,
    }
}
