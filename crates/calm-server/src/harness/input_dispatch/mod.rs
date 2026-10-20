//! Canonical dispatch of an already-authorized, immutable submission. The
//! conversation transaction facade owns source/execution admission and claims;
//! this owner commits the crossing before native effects and the private report
//! before publishing a result. Missing returns remain frozen in the same snapshot.
mod end_transition;
mod hydrate;
pub(crate) use end_transition::{complete_once, execution_target_tx};

#[cfg(all(test, feature = "fixtures"))]
mod tests;

use calm_types::conversation::{
    ConversationExecutionProvenance, FrozenInputInvocation, InputAttempt, InputAttemptFact,
    InputAttemptId, InputCorrelationFact, InputDeliveryFact, InputInvocation, InputSubmissionId,
    LogicalTurnId, NativeInputAcceptance, SubmissionDispatchProof,
};
use calm_types::harness::HarnessPhaseTag;
use calm_types::runtime::AgentProvider;
use serde_json::Value;

use crate::conversation_inputs::InputSource;
use crate::db::sqlite::conversation::{
    RuntimeExecutionFence, begin_input_attempt_tx, current_input_carrier_tx,
    current_input_fact_carrier_tx, record_input_attempt_fact_tx, release_submission_tx,
    submission_identity_tx, submission_plan_tx,
};
use crate::db::{Repo, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::harness::backend::{
    InputAttemptReport, InputDelivery, NativeAcceptanceBasis, NonDispatchBasis, PlannerBackend,
};
use crate::planner_permission_mode::PlannerPermissionMode;
use provider::InputItem;
use provider::TurnModelSelection;

/// Transient dispatch material; deliberately not serializable. Its proof is
/// body-free, while its hydrated items live only across this one provider call.
struct PreparedInputDispatch {
    logical_turn_id: LogicalTurnId,
    proof: SubmissionDispatchProof,
    thread_id: String,
    items: Vec<InputItem>,
    source: InputSource,
    card: String,
    worker: String,
    fence: RuntimeExecutionFence,
    invocation: FrozenInputInvocation,
}

impl PreparedInputDispatch {
    /// A committed marker is a fact, not authority across a native gate. This
    /// owner retains the real issuer only for this one invocation.
    async fn admit(&self, repo: &dyn Repo) -> Result<()> {
        self.source.require_repo(repo)?;
        let (card, worker, source, expected, attempt, logical, thread) = (
            self.card.clone(),
            self.worker.clone(),
            self.source.clone(),
            self.fence.clone(),
            self.proof.attempt().clone(),
            self.logical_turn_id.clone(),
            self.thread_id.clone(),
        );
        write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
                let carrier = current_input_carrier_tx(tx, &card, &worker).await?;
                if RuntimeExecutionFence::from_carrier(&carrier) != expected {
                    return Err(CalmError::Conflict("native input execution changed".into()));
                }
                let stored = submission_plan_tx(tx, &attempt.submission_id).await?;
                let identity = submission_identity_tx(tx, &attempt.submission_id).await?;
                let (execution, runtime) = crate::conversation_inputs::authorize_execution_tx(
                    tx,
                    &source,
                    &carrier,
                    &stored.plan,
                )
                .await?;
                if identity.card_id != card
                    || identity.logical_turn_id != logical
                    || identity.execution != execution
                    || runtime.thread_id.as_deref() != Some(thread.as_str())
                    || !carrier
                        .checkpoint()
                        .attempt(&attempt.submission_id)
                        .is_some_and(|actual| {
                            actual.attempt == attempt
                                && matches!(actual.delivery, InputDeliveryFact::CrossingUnknown)
                        })
                {
                    return Err(CalmError::Conflict(
                        "native input no longer owns its original crossing".into(),
                    ));
                }
                Ok(())
            })
        })
        .await
    }
}

pub(crate) struct CommittedInputReport {
    pub report: InputAttemptReport,
    pub consumed: Vec<calm_types::conversation::AcceptedInputRef>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn initial(
    repo: &dyn Repo,
    source: impl Into<InputSource>,
    card_id: &str,
    worker_session_id: &str,
    submission_id: &InputSubmissionId,
    backend: &PlannerBackend,
    selection: &TurnModelSelection,
    permission: PlannerPermissionMode,
) -> Result<CommittedInputReport> {
    let source = source.into();
    source.require_repo(repo)?;
    let mut prepared = prepare(
        repo,
        &source,
        card_id,
        worker_session_id,
        submission_id,
        backend.provider(),
        FrozenInputInvocation {
            model: selection.model.clone(),
            effort: selection.effort.clone(),
            permission,
        },
    )
    .await?;
    let items = std::mem::take(&mut prepared.items);
    let selection = TurnModelSelection {
        model: prepared.invocation.model.clone(),
        effort: prepared.invocation.effort.clone(),
    };
    let report = backend
        .dispatch_input(
            &prepared.logical_turn_id,
            prepared.proof.attempt(),
            &prepared.thread_id,
            &prepared.proof,
            items,
            &selection,
            prepared.invocation.permission,
            &crate::harness::native_admission::NativePromptAdmission::new(
                || prepared.source.check_current(),
                || Box::pin(prepared.admit(repo)),
            ),
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
    source: impl Into<InputSource>,
    card_id: &str,
    worker_session_id: &str,
    submission_id: &InputSubmissionId,
    provider: AgentProvider,
    invocation: FrozenInputInvocation,
) -> Result<PreparedInputDispatch> {
    let card_id = card_id.to_owned();
    let source = source.into();
    source.require_repo(repo)?;
    let worker_session_id = worker_session_id.to_owned();
    let submission_id = submission_id.clone();
    // write_in_tx_typed returns only after the protected write commits. A
    // cancelled provider future therefore leaves a real CrossingUnknown marker.
    write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let mut carrier = current_input_carrier_tx(tx, &card_id, &worker_session_id).await?;
            let identity = submission_identity_tx(tx, &submission_id).await?;
            let stored = submission_plan_tx(tx, &submission_id).await?;
            // A previous claim/provenance is not a grant for a fresh crossing,
            // including recovery of a claimed submission with no attempt yet.
            let (execution, runtime) = crate::conversation_inputs::authorize_execution_tx(
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
            if identity.ordinal != 0 || phase != HarnessPhaseTag::IssuingTurn {
                return Err(CalmError::Conflict(
                    "submission no longer targets this execution phase".into(),
                ));
            }
            if carrier
                .snapshot()
                .get("pending_rewind")
                .is_some_and(|value| !value.is_null())
            {
                return Err(CalmError::Conflict(
                    "canonical input cannot dispatch with pending history replacement".into(),
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
                native_turn_id: None,
                local_turn_id: None,
                rpc_request_id: None,
            };
            begin_input_attempt_tx(tx, &mut carrier, attempt.clone(), correlation, invocation)
                .await?;
            let invocation = match &carrier
                .checkpoint()
                .attempt(&attempt.submission_id)
                .ok_or_else(|| CalmError::Internal("committed crossing missing".into()))?
                .invocation
            {
                InputInvocation::Recorded { configuration } => configuration.clone(),
                InputInvocation::PriorInvocationNotRecorded => {
                    return Err(CalmError::Conflict(
                        "new crossing lacks its invocation".into(),
                    ));
                }
            };
            Ok(PreparedInputDispatch {
                logical_turn_id: identity.logical_turn_id,
                proof,
                thread_id,
                items,
                source,
                card: card_id,
                worker: worker_session_id,
                fence: RuntimeExecutionFence::from_carrier(&carrier),
                invocation,
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
    let consumed = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let mut carrier =
                current_input_fact_carrier_tx(tx, &card_id, &worker_session_id).await?;
            record_input_attempt_fact_tx(tx, &mut carrier, fact.clone()).await?;
            if may_release {
                release_submission_tx(tx, &mut carrier, &fact.attempt).await?;
            }
            if may_release {
                Ok(vec![])
            } else {
                submission_plan_tx(tx, &fact.attempt.submission_id)
                    .await?
                    .plan
                    .input_refs()
                    .map_err(|error| CalmError::Conflict(error.into()))
            }
        })
    })
    .await?;
    Ok(CommittedInputReport { report, consumed })
}

fn report_fact(expected: &InputAttempt, report: &InputAttemptReport) -> Result<InputAttemptFact> {
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
            }
            .into(),
        },
        InputDelivery::Accepted(receipt) => InputDeliveryFact::Accepted {
            basis: match receipt.basis() {
                NativeAcceptanceBasis::CodexTurnStart => "codex_turn_start",
            }
            .into(),
            evidence: NativeInputAcceptance::NativeTurn {
                native_turn_id: receipt.native_turn_id().into(),
            },
        },
        InputDelivery::Unknown(_) => InputDeliveryFact::Unknown,
    };
    Ok(InputAttemptFact {
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

/// Exercise the production renderer from issuance tests after reloading the frozen plan.
#[cfg(test)]
pub(crate) async fn hydrate_for_test(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &str,
    plan: &calm_types::conversation::SubmissionRenderPlan,
) -> Result<Vec<InputItem>> {
    hydrate::hydrate(tx, card, plan).await
}
