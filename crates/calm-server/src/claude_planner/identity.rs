//! Claude owns native identity recovery. A validated exact replay already proves that
//! this thread's session existed, even if its earlier init bind never committed. A
//! recorded `--session-id` launch that nothing confirms is never launched again.
use super::session::ClaudePlannerSessionParams;
use crate::db::sqlite::claude_session_launch;
use crate::db::sqlite::conversation::{current_input_fact_carrier_tx, submission_identity_tx};
use crate::error::{CalmError, Result};
use crate::harness::backend::{ExecutionEndScope, InputAcknowledgementBasis};
use crate::session_projection_repo::{AgentProvider, ThreadAttribution};
use calm_types::conversation::{
    ConversationExecutionProvenance, InputDeliveryFact, NativeInputAcceptance,
};
use calm_types::worker::{WorkerSession, WorkerSessionId};
use sqlx::{Sqlite, Transaction};

/// Recovery repairs identity only. It grants no execution authority and leaves
/// every input fact (including a subsequent Unknown attempt) untouched.
pub(super) async fn recover(params: &ClaudePlannerSessionParams) -> Result<bool> {
    let card = params.card_id.clone();
    let worker = params.worker_session_id.clone();
    let track = params.track_id.clone();
    crate::db::write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let Some(identity) = verified_replay_tx(tx, &card, &worker, &track).await? else {
                return Ok(false);
            };
            if identity.row.agent_session_id.is_none() {
                bind_tx(tx, &identity.row, &identity.thread).await?;
            }
            Ok(true)
        })
    })
    .await
}

struct VerifiedIdentity {
    row: WorkerSession,
    thread: String,
}

/// One native-evidence validator for durable repair and read-only recovery declarations.
/// A failed live bind does not turn an exact Accepted replay back into uncertain identity.
async fn verified_replay_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    worker: &str,
    track: &str,
) -> Result<Option<VerifiedIdentity>> {
    let carrier = current_input_fact_carrier_tx(tx, card, worker).await?;
    let (binding, row) = crate::harness::profile::input_carrier_contract_tx(tx, &carrier).await?;
    if binding.provider != AgentProvider::Claude || row.track_id.as_str() != track {
        return Err(CalmError::Conflict(
            "Claude identity recovery crosses its carrier".into(),
        ));
    }
    if matches!(launch_tx(tx, &row, card, track).await?, Launch::Crossed) {
        return Err(CalmError::Conflict(
            "Claude launch evidence crosses its owner".into(),
        ));
    }
    let Some(thread) = row.thread_id.as_deref() else {
        return Ok(None);
    };
    if carrier
        .snapshot()
        .get("last_thread_id")
        .and_then(serde_json::Value::as_str)
        != Some(thread)
    {
        return Err(CalmError::Conflict(
            "Claude identity recovery crosses its thread".into(),
        ));
    }
    if let Some(bound) = row.agent_session_id.as_deref() {
        if bound != thread {
            return Err(CalmError::Conflict(
                "Claude identity binding changed during recovery".into(),
            ));
        }
        let thread = thread.to_owned();
        return Ok(Some(VerifiedIdentity { row, thread }));
    }
    for checkpoint in &carrier.checkpoint().attempts {
        let InputDeliveryFact::Accepted {
            basis,
            evidence: NativeInputAcceptance::Submission { submission_id },
        } = &checkpoint.delivery
        else {
            continue;
        };
        if basis != InputAcknowledgementBasis::ClaudeUserReplay.as_str()
            || submission_id != &checkpoint.attempt.submission_id
            || checkpoint.correlation.provider != AgentProvider::Claude
            || checkpoint.correlation.thread_id != thread
            || !matches!(
                ExecutionEndScope::from_correlation(&checkpoint.correlation),
                Some(ExecutionEndScope::LocalProcess { .. })
            )
        {
            continue;
        }
        let identity = submission_identity_tx(tx, submission_id).await?;
        let ConversationExecutionProvenance::LegacySingleUserDeployment {
            provider, profile, ..
        } = identity.execution;
        if identity.card_id != card
            || identity.ordinal != 0
            || provider != AgentProvider::Claude
            || profile != binding.profile
        {
            return Err(CalmError::Conflict(
                "Claude replay evidence crosses its original input".into(),
            ));
        }
        let thread = thread.to_owned();
        return Ok(Some(VerifiedIdentity { row, thread }));
    }
    Ok(None)
}

/// One attribution writer for live init and verified recovery. Preserve the
/// harness-owned active turn, row state, checkpoint and provider execution claim.
pub(super) async fn bind_tx(
    tx: &mut Transaction<'_, Sqlite>,
    row: &WorkerSession,
    thread: &str,
) -> Result<()> {
    crate::db::sqlite::session_bind_attribution_tx(
        tx,
        &row.id.0,
        ThreadAttribution {
            worker_session_id: row.id.0.clone(),
            provider: AgentProvider::Claude,
            thread_id: Some(thread.into()),
            session_id: Some(thread.into()),
            active_turn_id: row.active_turn_id.clone(),
        },
    )
    .await?;
    // The bind confirms the session, so no launch of it remains unconfirmed.
    claude_session_launch::delete(tx, &row.id.0).await?;
    Ok(())
}

/// What a session may rely on before it launches this thread's native session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Identity {
    /// The row's `agent_session_id` names the session.
    Bound,
    /// Verified replay evidence repaired the binding.
    Recovered,
    /// A recorded `--session-id` launch may have created the session; nothing confirms it.
    Unconfirmed,
    /// No unretired launch of this thread's session is recorded.
    New,
}

/// The launch evidence a row carries, read by every classifier below.
enum Launch {
    Absent,
    /// Recorded for another thread of this owner, or superseded by a bind.
    Stale,
    Unconfirmed(claude_session_launch::ClaudeSessionLaunch),
    /// Recorded for another card or Track than the row's owner.
    Crossed,
}

async fn launch_tx(
    tx: &mut Transaction<'_, Sqlite>,
    row: &WorkerSession,
    card: &str,
    track: &str,
) -> Result<Launch> {
    let Some(launch) = claude_session_launch::get(tx, &row.id.0).await? else {
        return Ok(Launch::Absent);
    };
    if launch.card_id != card
        || launch.track_id != track
        || row.card_id.as_ref().map(|card| card.as_str()) != Some(card)
        || row.track_id.as_str() != track
    {
        return Ok(Launch::Crossed);
    }
    Ok(
        if row.agent_session_id.is_none()
            && row.thread_id.as_deref() == Some(launch.thread_id.as_str())
        {
            Launch::Unconfirmed(launch)
        } else {
            Launch::Stale
        },
    )
}

/// Whether the row belongs to a canonical input carrier.
pub(super) fn canonical(row: &WorkerSession) -> bool {
    row.handle_state_json.as_ref().is_some_and(|snapshot| {
        snapshot
            .get(calm_types::conversation::CONVERSATION_INPUT_CHECKPOINT_KEY)
            .is_some()
    })
}

/// Classify from durable facts only: the binding, verified replay evidence and the launch
/// record. Native diagnostics, transcripts and Claude's files never name the session.
pub(super) async fn open(params: &ClaudePlannerSessionParams) -> Result<Identity> {
    let worker = WorkerSessionId(params.worker_session_id.clone());
    let row = params
        .repo
        .session_get(&worker)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("worker session {}", worker.0)))?;
    if row.agent_session_id.is_some() {
        return launch(params).await;
    }
    if canonical(&row) && recover(params).await? {
        return Ok(Identity::Recovered);
    }
    launch(params).await
}

/// Before a `New` spawn: the binding and the launch record, never recovery. Stale evidence
/// is removed; evidence crossing its owner refuses.
pub(super) async fn launch(params: &ClaudePlannerSessionParams) -> Result<Identity> {
    let worker = WorkerSessionId(params.worker_session_id.clone());
    let (card, track) = (params.card_id.clone(), params.track_id.clone());
    crate::db::write_in_tx_typed(params.repo.as_ref(), move |tx| {
        Box::pin(async move {
            let row = crate::db::sqlite::session_get_tx(tx, &worker)
                .await?
                .ok_or_else(|| CalmError::NotFound(format!("worker session {}", worker.0)))?;
            let bound = row.agent_session_id.is_some();
            match launch_tx(tx, &row, &card, &track).await? {
                Launch::Absent => Ok(if bound {
                    Identity::Bound
                } else {
                    Identity::New
                }),
                Launch::Unconfirmed(_) => Ok(Identity::Unconfirmed),
                Launch::Stale => {
                    claude_session_launch::delete(tx, &worker.0).await?;
                    Ok(if bound {
                        Identity::Bound
                    } else {
                        Identity::New
                    })
                }
                Launch::Crossed => Err(CalmError::Conflict(
                    "Claude launch evidence crosses its owner".into(),
                )),
            }
        })
    })
    .await
}

/// A crossed record remains blocked, but only a matching record licenses retirement.
enum RecoveryLaunch {
    Crossed,
    Unconfirmed(claude_session_launch::ClaudeSessionLaunch),
}

async fn recovery_launch_tx(
    tx: &mut Transaction<'_, Sqlite>,
    worker: &str,
) -> Result<Option<RecoveryLaunch>> {
    let Some(row) = crate::db::sqlite::session_get_tx(tx, &WorkerSessionId(worker.into())).await?
    else {
        return Ok(None);
    };
    if crate::db::sqlite::provider_execution::load(&mut **tx, worker)
        .await?
        .is_some()
    {
        return Ok(None);
    }
    let Some(card) = row.card_id.as_ref() else {
        return Ok(None);
    };
    match launch_tx(tx, &row, card.as_str(), row.track_id.as_str()).await? {
        Launch::Crossed => Ok(Some(RecoveryLaunch::Crossed)),
        Launch::Unconfirmed(launch) => {
            if canonical(&row)
                && verified_replay_tx(tx, card.as_str(), worker, row.track_id.as_str())
                    .await?
                    .is_some()
            {
                return Ok(None);
            }
            Ok(Some(RecoveryLaunch::Unconfirmed(launch)))
        }
        Launch::Absent | Launch::Stale => Ok(None),
    }
}

/// Read-only: a live execution is not yet recovery, and verified replay is not uncertainty.
pub(crate) async fn unconfirmed_tx(tx: &mut Transaction<'_, Sqlite>, worker: &str) -> Result<bool> {
    Ok(recovery_launch_tx(tx, worker).await?.is_some())
}

/// Only matching launch ownership permits an explicit fresh-session recovery.
pub(crate) async fn recoverable_tx(tx: &mut Transaction<'_, Sqlite>, worker: &str) -> Result<bool> {
    let Some(RecoveryLaunch::Unconfirmed(launch)) = recovery_launch_tx(tx, worker).await? else {
        return Ok(false);
    };
    let alias =
        crate::provider_execution::AliasPrefix::planner().alias(worker, &launch.execution_id);
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspace_executions WHERE execution_id=?1 AND alias=?2 AND track_id=?3)")
        .bind(&launch.execution_id).bind(alias).bind(&launch.track_id).fetch_one(&mut **tx).await?)
}

/// The admission of a turn spawn. A `--session-id` launch's record is written in the
/// admitting transaction, so it commits exactly when the launch is admitted: a write
/// failure admits nothing, and no process starts before its record is durable.
pub(super) struct LaunchAdmission {
    pub(super) intent: crate::provider_execution::PreparedIntent,
    pub(super) launch: Option<claude_session_launch::ClaudeSessionLaunch>,
}

impl LaunchAdmission {
    pub(super) fn new(
        params: &ClaudePlannerSessionParams,
        intent: workspace_runtime::execution::ExecutionIntent,
        launch: Option<(&str, &str)>,
    ) -> Self {
        Self {
            intent: crate::provider_execution::PreparedIntent(intent),
            launch: launch.map(|(thread, execution_id)| {
                claude_session_launch::ClaudeSessionLaunch {
                    worker_session_id: params.worker_session_id.clone(),
                    card_id: params.card_id.clone(),
                    track_id: params.track_id.clone(),
                    thread_id: thread.into(),
                    execution_id: execution_id.into(),
                }
            }),
        }
    }
}

#[async_trait::async_trait]
impl crate::provider_execution::Admission for LaunchAdmission {
    async fn authorize_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        self.intent.authorize_tx(tx).await
    }
    async fn intent_tx(
        self,
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<workspace_runtime::execution::ExecutionIntent> {
        if let Some(launch) = &self.launch {
            claude_session_launch::insert(tx, launch).await?;
        }
        self.intent.intent_tx(tx).await
    }
}

/// A refused execution retires its own launch only while its durable claim is still held:
/// the release payload never reached the process, so Claude never ran.
pub(super) async fn retire_unreleased_launch(
    repo: &dyn crate::db::Repo,
    worker: &str,
    execution_id: &str,
) -> Result<()> {
    let (worker, execution_id) = (worker.to_owned(), execution_id.to_owned());
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let held = crate::db::sqlite::provider_execution::load(&mut **tx, &worker)
                .await?
                .is_some_and(|claim| {
                    claim.record().execution_id == execution_id
                        && claim.record().release
                            == crate::db::sqlite::provider_execution::Release::Held
                });
            if held {
                claude_session_launch::retire(tx, &worker, &execution_id).await?;
            }
            Ok(())
        })
    })
    .await
}
