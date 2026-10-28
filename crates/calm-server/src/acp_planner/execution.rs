//! ACP owns receipt completion; Engine alone establishes process settlement.
use super::{
    config::Registration,
    session::{self, SessionParams, Shared},
};
use crate::{
    error::{CalmError, Result},
    provider_execution::*,
};
use async_trait::async_trait;
use calm_truth::db::sqlite::provider_execution as claims;
use provider::{
    acp::{Connection, protocol},
    events::PlannerEvent,
};
use std::{
    path::Path,
    sync::{Arc, OnceLock},
};
pub const CONSUMER: &str = "acp-planner";
#[derive(Default)]
pub(crate) struct Owner {
    repo: OnceLock<Arc<dyn crate::db::Repo>>,
    registry: OnceLock<crate::harness::registry::WeakHarnessRegistry>,
}
impl std::fmt::Debug for Owner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AcpExecutionOwner")
    }
}
impl Owner {
    pub(crate) fn repo(&self) -> Result<&Arc<dyn crate::db::Repo>> {
        self.repo
            .get()
            .ok_or_else(|| conflict("ACP completion repository unavailable"))
    }
    pub(crate) fn bind(&self, repo: Arc<dyn crate::db::Repo>) {
        let _ = self.repo.set(repo);
    }
    pub(crate) fn bind_registry(&self, registry: crate::harness::HarnessRegistry) {
        let _ = self.registry.set(registry.downgrade());
    }
}
pub(crate) fn owner() -> (ProviderExecution, Arc<Owner>) {
    let owner = Arc::new(Owner::default());
    (
        ProviderExecution {
            owner: owner.clone(),
            drivers: Default::default(),
        },
        owner,
    )
}
/// Facts observed by the live driver, never reconstructed by recovery.
pub struct End {
    pub(super) shared: Arc<Shared>,
    pub(super) receipt: crate::db::sqlite::AcpSubmission,
    pub(super) events: Vec<PlannerEvent>,
    pub(super) turn: serde_json::Value,
}
#[async_trait]
impl ProviderExecutionOwner for Owner {
    fn consumer(&self) -> &'static str {
        CONSUMER
    }
    fn alias_prefix(&self) -> crate::provider_execution::AliasPrefix {
        crate::provider_execution::AliasPrefix::planner()
    }
    fn alias(&self, session: &str, id: &str) -> String {
        self.alias_prefix().alias(session, id)
    }
    async fn complete(
        &self,
        session: &str,
        record: &Record,
        evidence: CompletionEvidence,
        known: Option<KnownEnd>,
    ) -> Result<()> {
        let turn_id = match &record.purpose {
            Purpose::Turn { turn_id } | Purpose::Compaction { turn_id } => turn_id,
            Purpose::RewindCheck => return Err(conflict("wrong ACP purpose")),
        };
        // Validate purpose and result before any credential, receipt or harness effect.
        match (&record.purpose, &known) {
            (Purpose::Turn { .. }, Some(KnownEnd::AcpTurn(end))) => {
                if end.receipt.worker_session_id != session || end.receipt.turn_id != *turn_id {
                    return Err(conflict("ACP completion identity changed"));
                }
            }
            (Purpose::Turn { .. }, None | Some(KnownEnd::AcpRefused))
            | (Purpose::Compaction { .. }, None | Some(KnownEnd::AcpCompaction)) => {}
            _ => return Err(conflict("wrong ACP completion result")),
        }
        let repo = self
            .repo
            .get()
            .ok_or_else(|| conflict("ACP repository unavailable"))?;
        let pool = repo.sqlite_db().ok_or(CalmError::ExecutionUnavailable)?;
        // One attempt inside ProviderExecution::finish's 10s budget. The driver's
        // 45s known-result retry has already finished before it enters this port.
        let worker = session.to_owned();
        let execution = record.execution_id.clone();
        let owned = crate::db::write_in_tx_typed(repo.as_ref(), move |tx| {
            Box::pin(async move {
                Ok(claims::revoke_credential_for_execution(tx, &worker, &execution).await?)
            })
        })
        .await?;
        if !owned {
            return Ok(());
        }
        if matches!(record.purpose, Purpose::Compaction { .. }) {
            // Maintenance has neither an input receipt nor a durable turn outcome.
            // Only recovery projects loss; the live compact driver retains its result.
            if known.is_none()
                && let Some(harness) = self.registry.get().and_then(|r| r.get(&session.to_owned()))
            {
                harness
                    .complete_provider_execution(
                        &execution_id(record)?,
                        turn_id,
                        &record.purpose,
                        serde_json::Value::Null,
                    )
                    .await?;
            }
            return Ok(());
        }
        let receipt = if matches!(known, None | Some(KnownEnd::AcpRefused)) {
            crate::db::sqlite::acp_submission_recover_turn(&pool, session, turn_id).await?
        } else {
            crate::db::sqlite::acp_submission_for_turn(pool.read_pool(), session, turn_id).await?
        };
        let Some(receipt) = receipt else {
            // Setup can fail before creating a receipt; it has no prompt outcome.
            return Ok(());
        };
        let (mut turn, events, shared) = match known {
            Some(KnownEnd::AcpTurn(end)) => (end.turn, end.events, Some(end.shared)),
            None | Some(KnownEnd::AcpRefused) => (
                receipt
                    .outcome_json
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()?
                    .unwrap_or_else(|| session::unknown_outcome(turn_id)),
                Vec::new(),
                None,
            ),
            _ => return Err(conflict("wrong ACP completion result")),
        };
        // A completed receipt's CAS winner is authoritative on every retry.
        if let Some(json) = receipt.outcome_json.as_deref() {
            turn = serde_json::from_str(json)?;
        }
        if let Some(turn) = turn.as_object_mut() {
            turn.remove("items");
        }
        let row = repo
            .session_get(&calm_types::worker::WorkerSessionId(session.into()))
            .await?
            .ok_or_else(|| conflict("ACP owner missing"))?;
        crate::harness::turn_outcome::record(
            repo.as_ref(),
            session,
            row.card_id
                .as_ref()
                .ok_or_else(|| conflict("ACP card missing"))?
                .as_str(),
            row.track_id.as_str(),
            &receipt.thread_id,
            turn_id,
            &turn,
            if shared.is_some() && matches!(evidence, CompletionEvidence::Settled(_)) {
                crate::harness::turn_outcome::Confirmation::ProviderTerminal
            } else {
                crate::harness::turn_outcome::Confirmation::Unconfirmed
            },
        )
        .await?;
        if let Some(shared) = shared {
            shared.finish_execution(
                &record.execution_id,
                turn_id,
                &receipt.thread_id,
                events,
                turn,
            );
        } else if let Some(harness) = self.registry.get().and_then(|r| r.get(&session.to_owned())) {
            // This clears only this execution's live slot. It never creates a
            // normally-joined cleanup observation for an aborted/panicked driver.
            harness
                .complete_provider_execution(&execution_id(record)?, turn_id, &record.purpose, turn)
                .await?;
        }
        Ok(())
    }
}

pub(super) struct Process {
    pub commands: Vec<(String, String)>,
    pub io: Arc<crate::provider_execution::stdio::WriteProgress>,
    pub connection: Connection,
    pub capabilities: protocol::AgentCapabilities,
}
/// Validate even registrations read by an earlier binary before minting authority.
pub(super) fn validate_registration(registration: Registration<'_>) -> Result<()> {
    use crate::operation::stdin_payload::{KeySource, validate_key};
    for key in registration.config.env.keys() {
        validate_key(key, KeySource::Operator)?;
    }
    Ok(())
}
pub(super) async fn intent(
    params: &SessionParams,
    registration: Registration<'_>,
    token: &str,
    permission: crate::planner_permission_mode::PlannerPermissionMode,
    id: &workspace_runtime::execution::ExecutionId,
    dir: &Path,
) -> Result<(workspace_runtime::execution::ExecutionIntent, Vec<u8>)> {
    use crate::execution_environment::{PROXY_KEYS, proxy_value_is_secret};
    use crate::operation::stdin_payload::{KeySource, PayloadKeys};
    use std::{collections::BTreeMap, ffi::OsString};
    validate_registration(registration)?;
    let launch = registration.profile.launch(permission);
    // Merge first, preserving each final value's source. Profile/kernel values
    // override registrations, exactly as the native Command::envs path did.
    let mut env: BTreeMap<String, (OsString, bool)> = registration
        .config
        .env
        .iter()
        .map(|(k, v)| (k.clone(), (v.into(), true)))
        .collect();
    env.insert(
        "PATH".into(),
        (crate::kernel_bin_path::kernel_led_path()?.path, false),
    );
    for (key, value) in crate::mcp_server::wiring::card_mcp_env(&params.host.mcp_socket, token) {
        env.insert(key.into(), (value.into(), key == "NEIGE_MCP_TOKEN"));
    }
    for (key, value) in launch.env {
        env.insert((*key).into(), ((*value).into(), false));
    }
    let mut secrets = BTreeMap::new();
    let mut public = Vec::new();
    for (key, (value, secret)) in env {
        if key == "NEIGE_MCP_TOKEN"
            || secret
            || (PROXY_KEYS.contains(&key.as_str()) && proxy_value_is_secret(&key, &value))
        {
            secrets.insert(key, value);
        } else {
            public.push((key, value));
        }
    }
    let present = secrets.keys().map(String::as_str).collect::<Vec<_>>();
    let keys = PayloadKeys::new(&present, KeySource::Kernel)?;
    let payload = keys
        .render(|key| secrets.get(key).cloned())
        .map_err(|_| CalmError::BadRequest("invalid ACP release payload".into()))?;
    let args = registration
        .config
        .args
        .iter()
        .map(OsString::from)
        .chain(registration.profile.argv(Some(launch)).map(OsString::from))
        .collect();
    crate::provider_execution::launch::intent(
        params.host.provider_execution.owner.as_ref(),
        &params.worker_session_id,
        &params.track_id,
        &params.cwd,
        id,
        dir,
        &registration.config.command,
        args,
        public,
        &keys.read_prefix(),
        payload,
        calm_session::control::execution::AmbientEnvironment::None,
    )
    .await
}
