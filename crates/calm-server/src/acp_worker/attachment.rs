//! Worker attachment policy and generation-bound startup cleanup.
use super::{admission::Admission, lifecycle};
use crate::{
    acp_session::{
        attachment::{self, Role},
        host::AcpHost,
        process::Commands,
        server::CleanupWitness,
    },
    db::write_in_tx_typed,
    error::{CalmError, Result},
    model::now_ms,
    operation::SpawnCtx,
};
use calm_types::acp_worker::WorkerSelectionSource;
use provider::acp::{configuration::Configuration, protocol, usage::UsageTracker};
use std::sync::{Arc, atomic::Ordering};

pub(super) struct Attachment {
    pub native: String,
    pub client: provider::acp::Client,
    pub capabilities: protocol::AgentCapabilities,
    pub commands: Commands,
    pub cleanup: CleanupWitness,
    pub usage: UsageTracker,
}

pub(super) async fn attach(
    host: &Arc<AcpHost>,
    admission: &mut Admission,
    ctx: &SpawnCtx,
) -> Result<Attachment> {
    let state = admission.check(false).await?;
    let cleanup = StartupCleanup {
        owner: admission.clone(),
        ctx: ctx.clone(),
        witness: None,
        no_prior_native: state.native_session_id.is_none() && state.credential_generation.is_none(),
        armed: true,
        reason: "ACP Worker attachment interrupted".into(),
    };
    if let Some(target) = &state.read_only_target {
        let expected = crate::operation::task_verify_adapter::target::Expected {
            canonical_path: target.canonical_path.clone(),
            git_common_dir: target.git_common_dir.clone(),
            commit_sha: target.head.clone(),
        };
        let sampled =
            crate::operation::task_verify_adapter::target::sample(&admission.cwd, &expected)
                .await
                .map_err(|_| {
                    CalmError::Conflict(
                "worker-read-only-unsampled: original checkout could not be sampled before prompt"
                    .into(),
            )
                })?;
        if !crate::operation::task_verify_adapter::target::reasons(&sampled, &expected).is_empty() {
            return Err(CalmError::Conflict(
                "worker-read-only-changed: checkout changed before prompt".into(),
            ));
        }
        admission.check(false).await?;
    }
    let registration = host.registration(&state.requested_selection.provider)?;
    let selection = provider::TurnModelSelection {
        model: state
            .requested_selection
            .model
            .clone()
            .or(state.effective_model.clone()),
        effort: state
            .requested_selection
            .reasoning_effort
            .clone()
            .or(state.effective_effort.clone()),
    };
    let worker = admission.worker.clone();
    let cwd = admission.cwd.clone();
    let mut role = WorkerRole { cleanup, admission };
    let attached = attachment::attach(
        host,
        registration,
        attachment::Request {
            worker: &worker,
            cwd: &cwd,
            original_native: state.native_session_id.as_deref(),
            reuse_binding: false,
            selection: &selection,
        },
        &mut role,
    )
    .await;
    let attached = match attached {
        Ok(attached) => attached,
        Err(error) => {
            let error = CalmError::from(error);
            role.cleanup.reason = error.to_string();
            return Err(error);
        }
    };
    role.cleanup.armed = false;
    let server = attached.owned.as_ref().expect("attached ACP");
    Ok(Attachment {
        native: attached.native,
        usage: attached.usage,
        cleanup: attached.cleanup,
        client: server.process.connection.client.clone(),
        capabilities: server.process.capabilities.clone(),
        commands: server.process.commands.clone(),
    })
}

struct WorkerRole<'a> {
    admission: &'a mut Admission,
    cleanup: StartupCleanup,
}
#[async_trait::async_trait]
impl Role for WorkerRole<'_> {
    async fn check(&self, _: Option<&str>) -> Result<()> {
        self.admission.check(false).await?;
        if !self.admission.authorized.load(Ordering::Acquire) {
            return Err(CalmError::Conflict("ACP Worker is fenced".into()));
        }
        Ok(())
    }
    async fn mint(&mut self) -> Result<String> {
        let admission = &mut *self.admission;
        let owner = admission.clone();
        let token = write_in_tx_typed(admission.repo.as_ref(), move |tx| {
            Box::pin(async move {
                let state = owner.check_tx(tx, false).await?;
                let token = crate::mcp_server::wiring::mint_and_persist_card_token(
                    tx,
                    &owner.card,
                    &owner.worker,
                )
                .await?;
                let mut next = state.clone();
                next.credential_generation = Some(crate::mcp_server::auth::hash_token(&token));
                next.cleanup_confirmed = false;
                if !crate::db::sqlite::acp_worker_state_cas_tx(
                    tx,
                    &owner.worker,
                    &owner.attempt,
                    None,
                    &state,
                    &next,
                )
                .await?
                {
                    return Err(CalmError::Conflict(
                        "Worker credential generation changed".into(),
                    ));
                }
                Ok(token)
            })
        })
        .await?;
        admission.credential = Some(crate::mcp_server::auth::hash_token(&token));
        self.cleanup.owner = admission.clone();
        Ok(token)
    }
    fn pending(&mut self, witness: CleanupWitness) {
        self.cleanup.witness = Some(witness);
    }
    async fn bind(&mut self, _: &str) -> Result<()> {
        Ok(())
    }
    fn observe(&self, _: &Configuration) {}
    async fn publish(&mut self, native: &str, configuration: &Configuration) -> Result<()> {
        let admission = &*self.admission;
        let owner = admission.clone();
        let native_for_tx = native.to_owned();
        let configured = configuration.clone();
        write_in_tx_typed(admission.repo.as_ref(),move|tx|Box::pin(async move {
        let state=owner.check_tx(tx,false).await?;
        crate::acp_session::server::ensure_native_owner(&mut **tx,&state.requested_selection.provider,&owner.worker,&native_for_tx).await?;
        let mut next=state.clone();next.native_session_id=Some(native_for_tx.clone());next.cleanup_confirmed=false;
        next.effective_model=configured.category("model").map(|option|option.current_value.clone());next.effective_effort=configured.category("thought_level").map(|option|option.current_value.clone());
        next.selection_source.model=origin(state.requested_selection.model.as_ref(),next.effective_model.as_ref());next.selection_source.effort=origin(state.requested_selection.reasoning_effort.as_ref(),next.effective_effort.as_ref());
        if !crate::db::sqlite::acp_worker_state_cas_tx(tx,&owner.worker,&owner.attempt,None,&state,&next).await?{return Err(CalmError::Conflict("ACP Worker attachment lost ownership".into()))}
        sqlx::query("UPDATE worker_sessions SET agent_session_id=?2,state='running',liveness='alive',updated_at_ms=?3 WHERE id=?1 AND active_turn_id IS NULL").bind(&owner.worker).bind(native_for_tx).bind(now_ms()).execute(&mut **tx).await?;Ok(())
    })).await?;
        Ok(())
    }
}

/// Owns cleanup from before session/new or resume is written until publication succeeds.
/// Drop also covers cancellation while awaiting a setup reply. Missing grants prove nothing.
struct StartupCleanup {
    owner: Admission,
    ctx: SpawnCtx,
    witness: Option<CleanupWitness>,
    no_prior_native: bool,
    armed: bool,
    reason: String,
}
impl Drop for StartupCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let owner = self.owner.clone();
        let ctx = self.ctx.clone();
        let witness = self.witness.clone();
        let no_prior_native = self.no_prior_native;
        let reason = self.reason.clone();
        owner.authorized.store(false, Ordering::Release);
        tokio::spawn(async move {
            if let Some(witness) = witness {
                if !witness.confirmed() {
                    witness.generation.wait_stopped().await;
                }
                if !witness.confirmed() {
                    return;
                }
            } else if !no_prior_native {
                return;
            }
            let result = async {
                owner.revoke().await?;
                let captured = owner.clone();
                write_in_tx_typed(owner.repo.as_ref(), move |tx| Box::pin(async move {
                    let state = captured.check_tx(tx, true).await?;
                    if state.credential_generation != captured.credential {
                        return Err(CalmError::Conflict("Worker startup credential generation changed".into()));
                    }
                    let idle: bool = sqlx::query_scalar("SELECT active_turn_id IS NULL AND provider_execution_json IS NULL FROM worker_sessions WHERE id=?1")
                        .bind(&captured.worker).fetch_one(&mut **tx).await?;
                    if !idle || state.goal_submitted { return Err(CalmError::Conflict("Worker startup cleanup lost ownership".into())); }
                    let mut next = state.clone(); next.cleanup_confirmed = true;
                    if !crate::db::sqlite::acp_worker_state_cas_tx(tx, &captured.worker, &captured.attempt, None, &state, &next).await? {
                        return Err(CalmError::Conflict("Worker startup cleanup lost ownership".into()));
                    }
                    Ok(())
                })).await?;
                lifecycle::fail_and_finalize(&ctx, &owner.worker, owner.credential.clone(), &reason).await
            }.await;
            if let Err(error) = result {
                tracing::warn!(%error, "ACP Worker startup cleanup remains fenced");
            }
        });
    }
}
fn origin(request: Option<&String>, effective: Option<&String>) -> WorkerSelectionSource {
    if request.is_some() {
        WorkerSelectionSource::Explicit
    } else if effective.is_some() {
        WorkerSelectionSource::ProviderDefaultReported
    } else {
        WorkerSelectionSource::ProviderDefaultUnreported
    }
}
