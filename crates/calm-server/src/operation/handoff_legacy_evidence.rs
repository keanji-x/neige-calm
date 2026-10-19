//! One strict conversion of legacy business, transport, immutable proof and binding.
use super::handoff_activation::ActivationResult;
use super::handoff_coordinator::{Claim, Claims, CoordinatorInput, UserHandoffRequest};
use super::handoff_coordinator::{ResourceResume, Resources};
use super::handoff_cutover::CutoverResult;
use super::handoff_cutover::{ValidatedCutoverPlan, conflict};
use super::{Operation, Tx, TxOutput};
use crate::error::Result;
use crate::harness::HarnessSnapshot;
use crate::harness::handoff_pause::{PauseReceipt, PauseRequest, ReleaseOutcome};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(untagged)]
enum Thread {
    Absent(()),
    Proven(String),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Transport {
    card_id: String,
    track_id: String,
    #[serde(rename = "runtime_id")]
    session_id: String,
    cwd: String,
    snapshot: HarnessSnapshot,
    // Unlike Option<String>, this required field rejects absence.
    codex_thread_id: Thread,
}
#[derive(sqlx::FromRow)]
struct StoredProof {
    activation_operation_id: String,
    session_id: String,
    retired_source_session_id: String,
    retired_target_session_id: String,
    thread_id: Option<String>,
    token_hash: Option<String>,
}
enum Proof {
    Prepared,
    Proven {
        thread_id: String,
        token_hash: Option<String>,
    },
}
pub(super) enum LegacyEvidence {
    Unprepared,
    Resources {
        resources: Resources,
        output: TxOutput,
    },
    Activated {
        output: TxOutput,
    },
}
impl LegacyEvidence {
    pub(super) fn result(&self, session_id: &str) -> ActivationResult {
        match self {
            Self::Unprepared => ActivationResult::AcceptedPending,
            Self::Resources { resources, .. } => resources.result(None),
            Self::Activated { .. } => ActivationResult::Activated {
                session_id: session_id.into(),
            },
        }
    }
}

pub(super) async fn activation(
    tx: &mut Tx<'_>,
    cutover: &str,
    plan: &ValidatedCutoverPlan,
    activation: Option<&Operation>,
) -> Result<LegacyEvidence> {
    let stored = sqlx::query_as::<_, StoredProof>(concat!(
        "SELECT activation_operation_id,session_id,retired_source_session_id,",
        "retired_target_session_id,thread_id,token_hash FROM planner_handoff_activations ",
        "WHERE cutover_operation_id=?1",
    ))
    .bind(cutover)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(activation) = activation else {
        if stored.is_some() {
            return Err(conflict());
        }
        super::handoff_activation_sql::current_facts(tx, plan.clone(), cutover.into(), None, false)
            .await?;
        return Ok(LegacyEvidence::Unprepared);
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        cutover_operation_id: String,
    }
    let input: Input = serde_json::from_value(activation.payload.clone())?;
    if activation.kind != "handoff-activation"
        || input.cutover_operation_id != cutover
        || activation.idempotency_key.as_deref() != Some(cutover)
        || activation.payload_hash
            != crate::routes::idempotency_key::stable_payload_hash(&activation.payload)?
    {
        return Err(conflict());
    }
    let Some(output) = &activation.tx_output else {
        if stored.is_some() {
            return Err(conflict());
        }
        super::handoff_activation_sql::current_facts(tx, plan.clone(), cutover.into(), None, false)
            .await?;
        return Ok(LegacyEvidence::Unprepared);
    };
    let state: ActivationResult = serde_json::from_value(output.result.clone())?;
    let transport: Transport = serde_json::from_value(output.data.clone())?;
    let stored = stored.ok_or_else(conflict)?;
    let scope = &plan.scope;
    if stored.activation_operation_id != activation.id
        || stored.session_id != scope.new_session_id
        || stored.retired_source_session_id != scope.source_session_id
        || stored.retired_target_session_id != scope.target_session_id
        || transport.card_id != scope.target_card_id.as_str()
        || transport.track_id != scope.track_id.as_str()
        || transport.session_id != scope.new_session_id
        || transport.cwd.is_empty()
        || output.target_type != "card"
        || output.target_id.as_deref() != Some(scope.target_card_id.as_str())
    {
        return Err(conflict());
    }
    transport
        .snapshot
        .handoff_entries_checked()
        .ok_or_else(conflict)?;
    let proof = match (stored.thread_id, stored.token_hash) {
        (None, None) => Proof::Prepared,
        (Some(thread_id), token_hash)
            if !thread_id.is_empty() && token_hash.as_ref().is_none_or(|s| !s.is_empty()) =>
        {
            Proof::Proven {
                thread_id,
                token_hash,
            }
        }
        _ => return Err(conflict()),
    };
    let proven = match (&transport.codex_thread_id, &proof) {
        (Thread::Absent(()), Proof::Prepared) if transport.snapshot.last_thread_id.is_none() => {
            None
        }
        (Thread::Proven(thread), Proof::Proven { thread_id, .. })
            if !thread.is_empty()
                && thread == thread_id
                && transport.snapshot.last_thread_id.as_ref() == Some(thread) =>
        {
            Some(thread.clone())
        }
        _ => return Err(conflict()),
    };
    if calm_types::worker::WorkerProviderKind::from(&plan.configuration.conversation.provider)
        == calm_types::worker::WorkerProviderKind::Codex
        && matches!(
            &proof,
            Proof::Proven {
                token_hash: None,
                ..
            }
        )
    {
        return Err(conflict());
    }
    let resources = match (&state, &proven) {
        (ActivationResult::Preparing, None) => Resources::Prepared,
        (
            ActivationResult::Blocked {
                reason,
                resources_checkpointed: false,
            },
            None,
        ) => Resources::Blocked {
            reason: reason.clone(),
            resume: ResourceResume::Prepared,
        },
        (ActivationResult::ResourceRequestRecorded, None) => Resources::Unknown {
            reason: "Legacy native result unconfirmed; never resend".into(),
        },
        (ActivationResult::Unknown { reason }, None) => Resources::Unknown {
            reason: reason.clone(),
        },
        (ActivationResult::ResourcesCheckpointed { thread_id }, Some(thread))
            if thread_id == thread =>
        {
            Resources::Proven {
                thread_id: thread.clone(),
            }
        }
        (
            ActivationResult::Blocked {
                reason,
                resources_checkpointed: true,
            },
            Some(thread),
        ) => Resources::Blocked {
            reason: reason.clone(),
            resume: ResourceResume::Proven {
                thread_id: thread.clone(),
            },
        },
        (ActivationResult::NeedsRecovery { .. }, Some(thread)) => Resources::Proven {
            thread_id: thread.clone(),
        },
        (ActivationResult::NeedsRecovery { .. }, None) => Resources::Prepared,
        (ActivationResult::Activated { session_id }, Some(_))
            if session_id == &scope.new_session_id =>
        {
            // Immutable success survives later lawful ownership changes. Do not re-install.
            return Ok(LegacyEvidence::Activated {
                output: output.clone(),
            });
        }
        _ => return Err(conflict()),
    };
    let facts = super::handoff_activation_sql::current_facts(
        tx,
        plan.clone(),
        cutover.into(),
        Some(&activation.id),
        false,
    )
    .await?;
    facts.validate_output(output)?;
    let session = crate::db::sqlite::session_get_tx(tx, &scope.new_session_id.as_str().into())
        .await?
        .ok_or_else(conflict)?;
    match proof {
        Proof::Prepared
            if session.state
                == crate::session_projection_repo::WorkerSessionState::PendingActivation
                && session.thread_id.is_none()
                && session.mcp_token_hash.is_none() => {}
        Proof::Proven {
            thread_id,
            token_hash,
        } if session.state.is_active_authority()
            && session.thread_id.as_ref() == Some(&thread_id)
            && session.mcp_token_hash == token_hash =>
        {
            let valid: bool = sqlx::query_scalar(concat!(
    "SELECT EXISTS(SELECT 1 FROM cards c WHERE c.id=?1 AND (?2 IS NULL OR EXISTS(SELECT ",
    "1 FROM card_mcp_tokens t WHERE t.card_id=c.id AND t.hashed_token=?2)))",
))
                    .bind(scope.target_card_id.as_str()).bind(&token_hash).fetch_one(&mut **tx).await?;
            if !valid {
                return Err(conflict());
            }
        }
        _ => return Err(conflict()),
    }
    Ok(LegacyEvidence::Resources {
        resources,
        output: output.clone(),
    })
}

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InputV1 {
    request: UserHandoffRequest,
    source: PauseRequest,
    target: PauseRequest,
    plan_id: String,
    new_session_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressV1 {
    stage: LegacyStage,
    source_attempted: bool,
    target_attempted: bool,
    #[serde(deserialize_with = "required_nullable")]
    source_receipt: Option<PauseReceipt>,
    #[serde(deserialize_with = "required_nullable")]
    target_receipt: Option<PauseReceipt>,
    #[serde(deserialize_with = "required_nullable")]
    source_release: Option<ReleaseOutcome>,
    #[serde(deserialize_with = "required_nullable")]
    target_release: Option<ReleaseOutcome>,
    #[serde(deserialize_with = "required_nullable")]
    plan: Option<ValidatedCutoverPlan>,
    #[serde(deserialize_with = "required_nullable")]
    committed: Option<CutoverResult>,
    #[serde(deserialize_with = "required_nullable")]
    reason: Option<String>,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum LegacyStage {
    Reserved,
    SourceIntent,
    SourcePaused,
    TargetIntent,
    BothPaused,
    PlanRecorded,
    CommitIntent,
    Committed,
    Compensating,
    Finished,
    ReleasedNeedsRecovery,
}
fn claim(
    attempted: bool,
    receipt: Option<PauseReceipt>,
    outcome: Option<ReleaseOutcome>,
) -> Result<Claim> {
    match (attempted, receipt, outcome) {
        (false, None, None) => Ok(Claim::Untouched),
        (true, None, None) => Ok(Claim::Intent),
        (true, Some(receipt), None) => Ok(Claim::Paused { receipt }),
        (true, Some(receipt), Some(outcome)) => Ok(Claim::Released { receipt, outcome }),
        _ => Err(conflict()),
    }
}
// Version-one optional fields are validated once, before either projection or adoption.
fn decode_coordinator(op: &Operation) -> Result<(CoordinatorInput, Option<ProgressV1>)> {
    if op.kind != "handoff-coordinator" {
        return Err(conflict());
    }
    let old: InputV1 = serde_json::from_value(op.payload.clone())?;
    old.request.validate()?;
    old.source.validate().map_err(|_| conflict())?;
    old.target.validate().map_err(|_| conflict())?;
    if op.payload_hash != old.request.operation_key()?.payload_hash
        || !old.source.matches_binding(
            &old.request.source_card_id,
            &old.request.source_session_id,
            &old.request.track_id,
        )
        || !old.target.matches_binding(
            &old.request.target_card_id,
            &old.request.target_session_id,
            &old.request.track_id,
        )
        || old.plan_id.is_empty()
        || old.new_session_id.is_empty()
        || old.new_session_id == old.request.source_session_id
        || old.new_session_id == old.request.target_session_id
    {
        return Err(conflict());
    }
    let input = CoordinatorInput {
        version: 2,
        request: old.request,
        source: old.source,
        target: old.target,
        plan_id: old.plan_id,
        new_session_id: old.new_session_id,
    };
    let Some(output) = &op.tx_output else {
        return Ok((input, None));
    };
    if output.target_type != "track"
        || output.target_id.as_deref() != Some(input.request.track_id.as_str())
    {
        return Err(conflict());
    }
    let p: ProgressV1 = serde_json::from_value(output.result.clone())?;
    for (attempted, receipt, outcome, request) in [
        (
            p.source_attempted,
            &p.source_receipt,
            &p.source_release,
            &input.source,
        ),
        (
            p.target_attempted,
            &p.target_receipt,
            &p.target_release,
            &input.target,
        ),
    ] {
        claim(attempted, receipt.clone(), outcome.clone())?;
        if receipt.as_ref().is_some_and(|r| r.request() != request) {
            return Err(conflict());
        }
    }
    let source = p.source_receipt.is_some();
    let target = p.target_receipt.is_some();
    let valid = match p.stage {
        LegacyStage::Reserved => !p.source_attempted && !p.target_attempted,
        LegacyStage::SourceIntent => !source && !p.target_attempted,
        LegacyStage::SourcePaused => source && !p.target_attempted,
        LegacyStage::TargetIntent => source && !target,
        LegacyStage::BothPaused
        | LegacyStage::PlanRecorded
        | LegacyStage::CommitIntent
        | LegacyStage::Committed => source && target,
        LegacyStage::Compensating => true,
        LegacyStage::ReleasedNeedsRecovery => {
            [(p.source_attempted, &p.source_release), (p.target_attempted, &p.target_release)]
                .iter().all(|(attempted, outcome)| !attempted || outcome.is_some())
                && [&p.source_release, &p.target_release].iter().any(|outcome| matches!(outcome, Some(outcome) if !matches!(outcome, ReleaseOutcome::HealthyReleased)))
        },
        // Legacy Finished promised healthy release. The shared owner's broader
        // settlement contract must not reinterpret malformed legacy terminal facts.
        LegacyStage::Finished => [&p.source_release, &p.target_release]
            .iter()
            .all(|outcome| outcome.as_ref().is_none_or(|o| matches!(o, ReleaseOutcome::HealthyReleased)))
            && super::handoff_coordinator::SettledClaims::try_from(Claims {
            source: claim(
                p.source_attempted,
                p.source_receipt.clone(),
                p.source_release.clone(),
            )?,
            target: claim(
                p.target_attempted,
                p.target_receipt.clone(),
                p.target_release.clone(),
            )?,
        })
        .is_ok(),
    };
    if !valid
        || (p.plan.is_some()
            && matches!(
                p.stage,
                LegacyStage::Reserved
                    | LegacyStage::SourceIntent
                    | LegacyStage::SourcePaused
                    | LegacyStage::TargetIntent
                    | LegacyStage::BothPaused
            ))
        || (p.committed.is_some() != (p.stage == LegacyStage::Committed))
        || (matches!(
            p.stage,
            LegacyStage::PlanRecorded | LegacyStage::CommitIntent | LegacyStage::Committed
        ) && p.plan.is_none())
        || (!matches!(
            p.stage,
            LegacyStage::Compensating | LegacyStage::Finished | LegacyStage::ReleasedNeedsRecovery
        ) && (p.source_release.is_some() || p.target_release.is_some()))
    {
        return Err(conflict());
    }
    if let Some(plan) = &p.plan {
        let scope = &plan.scope;
        if scope.source_pause.request() != &input.source
            || scope.target_pause.request() != &input.target
            || scope.plan_id != input.plan_id
            || scope.new_session_id != input.new_session_id
            || scope.user_audit_id != input.request.user_audit_id
            || scope.track_id != input.request.track_id
            || scope.source_card_id != input.request.source_card_id
            || scope.target_card_id != input.request.target_card_id
            || scope.source_session_id != input.request.source_session_id
            || scope.target_session_id != input.request.target_session_id
        {
            return Err(conflict());
        }
    }
    Ok((input, Some(p)))
}

pub(super) struct CoordinatorEvidence {
    pub input: CoordinatorInput,
    pub state: CoordinatorState,
}
pub(super) enum CoordinatorState {
    Precommit { claims: Box<Claims>, reason: String },
    Committed { result: Box<CutoverResult> },
}
async fn precommit_claim(
    tx: &mut Tx<'_>,
    request: &PauseRequest,
) -> Result<crate::harness::handoff_pause::PauseQuery> {
    let fact = crate::db::handoff_pause::query_handoff_pause_tx(tx, request.clone()).await?;
    if matches!(
        &fact,
        crate::harness::handoff_pause::PauseQuery::Recorded(
            crate::harness::handoff_pause::PauseState::ConsumedByHandoff { .. }
        )
    ) {
        return Err(conflict());
    }
    Ok(fact)
}

pub(super) async fn coordinator(
    tx: &mut Tx<'_>,
    op: &Operation,
    request: &UserHandoffRequest,
) -> Result<CoordinatorEvidence> {
    let (input, progress) = decode_coordinator(op)?;
    if input.request != *request {
        return Err(conflict());
    }
    if progress.is_none() {
        for request in [&input.source, &input.target] {
            precommit_claim(tx, request).await?;
        }
    }
    let mut reason = "Legacy original claims require explicit settlement".to_string();
    if let Some(p) = progress {
        if let Some(diagnostic) = &p.reason {
            reason = diagnostic.clone();
        }
        if p.committed.is_some() || p.stage == LegacyStage::CommitIntent {
            let recorded = super::handoff_cutover::query_recorded_tx(
                tx,
                p.plan.as_ref().ok_or_else(conflict)?,
            )
            .await?;
            if p.committed
                .as_ref()
                .is_some_and(|expected| recorded.as_ref() != Some(expected))
            {
                return Err(conflict());
            }
            if let Some(result) = recorded {
                return Ok(CoordinatorEvidence {
                    input,
                    state: CoordinatorState::Committed {
                        result: Box::new(result),
                    },
                });
            }
        }
        for (request, receipt, outcome) in [
            (&input.source, &p.source_receipt, &p.source_release),
            (&input.target, &p.target_receipt, &p.target_release),
        ] {
            let fact = precommit_claim(tx, request).await?;
            if let Some(receipt) = receipt {
                let crate::harness::handoff_pause::PauseQuery::Recorded(state) = &fact else {
                    return Err(conflict());
                };
                if state.receipt() != Some(receipt) {
                    return Err(conflict());
                }
                if let Some(outcome) = outcome
                    && !matches!(state, crate::harness::handoff_pause::PauseState::Released { outcome: actual, .. } if actual == outcome)
                {
                    return Err(conflict());
                }
            }
        }
    }
    // No old flag can prove that a command beyond its intent seam will never arrive.
    // Resolve both original requests, retaining typed release health in the shared owner.
    Ok(CoordinatorEvidence {
        input,
        state: CoordinatorState::Precommit {
            claims: Box::new(Claims {
                source: Claim::Intent,
                target: Claim::Intent,
            }),
            reason,
        },
    })
}
