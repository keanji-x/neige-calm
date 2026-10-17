//! Canonical dispatch of an already-authorized, immutable submission. The
//! conversation transaction facade owns source/execution admission and claims;
//! this owner commits the crossing before native effects and the private report
//! before publishing a result. Missing returns remain frozen in the same snapshot.
mod end_transition;
mod hydrate;
pub(crate) use end_transition::complete_once;

#[cfg(all(test, feature = "fixtures"))]
mod tests;

use calm_types::conversation::{
    ConversationExecutionProvenance, InputAttempt, InputAttemptCheckpoint, InputAttemptId,
    InputCorrelationFact, InputDeliveryFact, InputSubmissionId, LogicalTurnId,
    NativeInputAcceptance, SubmissionDispatchProof,
};
use calm_types::harness::HarnessPhaseTag;
use calm_types::runtime::AgentProvider;
use serde_json::Value;

use crate::auth::HumanInputSource;
use crate::codex_appserver::InputItem;
use crate::db::sqlite::conversation::{
    begin_input_attempt_tx, current_input_carrier_tx, record_input_attempt_fact_tx,
    release_submission_tx, submission_identity_tx, submission_plan_tx,
};
use crate::db::{Repo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::harness::backend::{
    BackendRewind, InputAttemptReport, InputDelivery, NativeAcceptanceBasis, NonDispatchBasis,
    PlannerBackend,
};
use crate::planner_model::TurnModelSelection;
use crate::planner_permission_mode::PlannerPermissionMode;

/// Transient dispatch material; deliberately not serializable. Its proof is
/// body-free, while its hydrated items live only across this one provider call.
struct PreparedInputDispatch {
    logical_turn_id: LogicalTurnId,
    proof: SubmissionDispatchProof,
    thread_id: String,
    items: Vec<InputItem>,
}

#[derive(Clone)]
enum Target {
    Initial,
    Supplemental { native_turn_id: String },
}

pub(crate) struct CommittedInputReport {
    pub report: InputAttemptReport,
    pub snapshot: Value,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn initial(
    repo: &dyn Repo,
    source: &HumanInputSource,
    card_id: &str,
    worker_session_id: &str,
    submission_id: &InputSubmissionId,
    backend: &PlannerBackend,
    selection: &TurnModelSelection,
    permission: PlannerPermissionMode,
    rewind: Option<&BackendRewind>,
) -> Result<CommittedInputReport> {
    let prepared = prepare(
        repo,
        source,
        card_id,
        worker_session_id,
        submission_id,
        backend.provider(),
        Target::Initial,
        rewind,
    )
    .await?;
    let report = backend
        .dispatch_input(
            &prepared.logical_turn_id,
            prepared.proof.attempt(),
            &prepared.thread_id,
            &prepared.proof,
            prepared.items,
            selection,
            permission,
            rewind,
        )
        .await;
    settle(
        repo,
        card_id,
        worker_session_id,
        prepared.proof.attempt(),
        report,
    )
    .await
}

pub(crate) async fn supplemental(
    repo: &dyn Repo,
    source: &HumanInputSource,
    card_id: &str,
    worker_session_id: &str,
    submission_id: &InputSubmissionId,
    backend: &PlannerBackend,
    expected_native_turn_id: &str,
) -> Result<CommittedInputReport> {
    if !backend.supports_steer() {
        return Err(CalmError::Conflict(
            "this provider does not support supplemental input".into(),
        ));
    }
    let prepared = prepare(
        repo,
        source,
        card_id,
        worker_session_id,
        submission_id,
        backend.provider(),
        Target::Supplemental {
            native_turn_id: expected_native_turn_id.into(),
        },
        None,
    )
    .await?;
    let report = backend
        .dispatch_supplemental(
            &prepared.logical_turn_id,
            prepared.proof.attempt(),
            &prepared.thread_id,
            expected_native_turn_id,
            &prepared.proof,
            prepared.items,
        )
        .await;
    settle(
        repo,
        card_id,
        worker_session_id,
        prepared.proof.attempt(),
        report,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn prepare(
    repo: &dyn Repo,
    source: &HumanInputSource,
    card_id: &str,
    worker_session_id: &str,
    submission_id: &InputSubmissionId,
    provider: AgentProvider,
    target: Target,
    rewind: Option<&BackendRewind>,
) -> Result<PreparedInputDispatch> {
    let card_id = card_id.to_owned();
    let source = source.clone();
    let worker_session_id = worker_session_id.to_owned();
    let submission_id = submission_id.clone();
    let rewind = rewind.map(serde_json::to_value).transpose()?;
    // write_in_tx_typed returns only after the protected write commits. A
    // cancelled provider future therefore leaves a real CrossingUnknown marker.
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let mut carrier = current_input_carrier_tx(tx, &card_id, &worker_session_id).await?;
            let identity = submission_identity_tx(tx, &submission_id).await?;
            let stored = submission_plan_tx(tx, &submission_id).await?;
            // A previous claim/provenance is not a grant for a fresh crossing,
            // including recovery of a claimed submission with no attempt yet.
            let (execution, runtime) = crate::conversation_inputs::authorize_human_execution_tx(
                tx,
                &source,
                &carrier,
                &stored.plan,
            )
            .await?;
            let ConversationExecutionProvenance::LegacySingleUserDeployment {
                provider: frozen_provider,
                ..
            } = &identity.execution;
            if identity.card_id != card_id
                || *frozen_provider != provider
                || execution != identity.execution
                || carrier.checkpoint().active.as_ref().is_none_or(|active| {
                    active.logical_turn_id != identity.logical_turn_id
                        || !active.submission_ids.contains(&submission_id)
                })
            {
                return Err(CalmError::Conflict(
                    "submission execution provenance or carrier binding changed".into(),
                ));
            }
            let phase: HarnessPhaseTag =
                serde_json::from_value(carrier.snapshot()["phase"].clone())?;
            let expected_turn = match target {
                Target::Initial
                    if identity.ordinal == 0 && phase == HarnessPhaseTag::IssuingTurn =>
                {
                    None
                }
                Target::Supplemental { native_turn_id }
                    if identity.ordinal > 0
                        && phase == HarnessPhaseTag::TurnRunning
                        && !native_turn_id.is_empty()
                        && carrier.snapshot()["last_turn_id"].as_str()
                            == Some(native_turn_id.as_str()) =>
                {
                    Some(native_turn_id)
                }
                _ => {
                    return Err(CalmError::Conflict(
                        "submission no longer targets this execution phase".into(),
                    ));
                }
            };
            let stored_rewind = carrier
                .snapshot()
                .get("pending_rewind")
                .filter(|value| !value.is_null());
            if stored_rewind != rewind.as_ref() {
                return Err(CalmError::Conflict(
                    "dispatch rewind differs from its durable checkpoint".into(),
                ));
            }
            let thread_id = carrier
                .snapshot()
                .get("last_thread_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| CalmError::Conflict("dispatch has no native thread".into()))?
                .to_owned();
            if runtime.thread_id.as_deref() != Some(thread_id.as_str()) {
                return Err(CalmError::Conflict(
                    "dispatch thread differs from its current runtime binding".into(),
                ));
            }
            // This exact revision read is in the marker transaction. A body-free
            // plan's digest alone cannot reveal human-only private erasure.
            let items = hydrate::hydrate(tx, &card_id, &stored.plan).await?;
            let attempt = InputAttempt {
                submission_id,
                attempt_id: InputAttemptId::parse(&crate::model::new_id())
                    .map_err(|error| CalmError::Internal(error.to_string()))?,
            };
            let proof = SubmissionDispatchProof::from_stored_plan(
                attempt.clone(),
                &stored.plan,
                stored.original_digest,
            )
            .map_err(CalmError::Conflict)?;
            let correlation = InputCorrelationFact {
                provider: provider.clone(),
                thread_id: thread_id.clone(),
                // Provider setup can still refuse before exposing a native
                // session. Its private report refines this when actually known.
                native_session_id: None,
                native_turn_id: expected_turn,
                local_turn_id: None,
                rpc_request_id: None,
            };
            begin_input_attempt_tx(tx, &mut carrier, attempt, correlation).await?;
            Ok(PreparedInputDispatch {
                logical_turn_id: identity.logical_turn_id,
                proof,
                thread_id,
                items,
            })
        })
    })
    .await
}

async fn settle(
    repo: &dyn Repo,
    card_id: &str,
    worker_session_id: &str,
    expected_attempt: &InputAttempt,
    report: InputAttemptReport,
) -> Result<CommittedInputReport> {
    let fact = report_fact(expected_attempt, &report)?;
    let may_release = matches!(fact.delivery, InputDeliveryFact::NotDispatched { .. });
    let card_id = card_id.to_owned();
    let worker_session_id = worker_session_id.to_owned();
    let snapshot = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let mut carrier = current_input_carrier_tx(tx, &card_id, &worker_session_id).await?;
            record_input_attempt_fact_tx(tx, &mut carrier, fact.clone()).await?;
            if may_release {
                release_submission_tx(tx, &mut carrier, &fact.attempt).await?;
            }
            Ok(carrier.snapshot().clone())
        })
    })
    .await?;
    Ok(CommittedInputReport { report, snapshot })
}

fn report_fact(
    expected: &InputAttempt,
    report: &InputAttemptReport,
) -> Result<InputAttemptCheckpoint> {
    let correlation = report.delivery().correlation();
    if report.attempt() != expected || correlation.client_id() != expected.submission_id.as_str() {
        return Err(CalmError::Conflict(
            "provider report names another input attempt".into(),
        ));
    }
    let delivery = match report.delivery() {
        InputDelivery::NotDispatched(evidence) => InputDeliveryFact::NotDispatched {
            basis: match evidence.basis() {
                NonDispatchBasis::LocalPreflight => "local_preflight",
                NonDispatchBasis::TransportBeforeSend => "transport_before_send",
                NonDispatchBasis::Unsupported => "unsupported",
            }
            .into(),
        },
        InputDelivery::Accepted(receipt) => InputDeliveryFact::Accepted {
            basis: match receipt.basis() {
                NativeAcceptanceBasis::CodexTurnStart => "codex_turn_start",
                NativeAcceptanceBasis::CodexTurnSteer => "codex_turn_steer",
            }
            .into(),
            evidence: NativeInputAcceptance::NativeTurn {
                native_turn_id: receipt.native_turn_id().into(),
            },
        },
        InputDelivery::Unknown(_) => InputDeliveryFact::Unknown,
    };
    Ok(InputAttemptCheckpoint {
        attempt: expected.clone(),
        correlation: InputCorrelationFact {
            provider: correlation.provider().clone(),
            thread_id: correlation.thread_id().into(),
            native_session_id: correlation.native_session_id().map(str::to_owned),
            native_turn_id: correlation.native_turn_id().map(str::to_owned),
            local_turn_id: correlation.local_turn_id().map(str::to_owned),
            rpc_request_id: correlation.rpc_request_id(),
        },
        delivery,
    })
}
