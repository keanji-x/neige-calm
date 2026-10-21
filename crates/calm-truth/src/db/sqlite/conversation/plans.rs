//! Immutable rendering and private erasure. No prompt/body archive is introduced.
use super::digest;
use crate::error::{CalmError, Result};
use calm_types::conversation::{
    AcceptedInputRef, ConversationExecutionProvenance, ConversationMessageId,
    FrozenHumanAttribution, InputCorrelationFact, InputDeliveryFact, InputSubmissionId,
    LogicalTurnId, MessageAuthor, MessageRevisionRef, PendingInput, PrivateHistoryPayload,
    SubmissionDispatchProof, SubmissionRenderDigest, SubmissionRenderInput, SubmissionRenderPlan,
    SubmissionRenderPlanRef, SubmissionRenderPlanVersion, SubmissionRenderSegment,
    SystemObservationId, SystemRenderContext,
};
use sqlx::{Row, Sqlite, Transaction};
use std::num::NonZeroU32;

#[derive(Clone, Debug, PartialEq)]
pub struct StoredSubmissionPlan {
    pub plan: SubmissionRenderPlan,
    pub original_digest: SubmissionRenderDigest,
}

/// Construct attribution from canonical provenance and the registered author,
/// never the caller or the source which may later authorize execution. This is
/// a body-free plan; the claim owner still validates this exact pending prefix,
/// current revisions and execution authority in the same protected transaction.
pub async fn pending_plan_for_prefix_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &super::CurrentInputCarrier,
    input_count: NonZeroU32,
    contexts: Vec<SystemRenderContext>,
) -> Result<SubmissionRenderPlan> {
    let count = usize::try_from(input_count.get())
        .map_err(|_| CalmError::Conflict("human input prefix is too large"))?;
    let entries = carrier
        .checkpoint()
        .pending
        .get(..count)
        .ok_or(CalmError::Conflict("pending human input prefix changed"))?;
    let mut segments = contexts
        .into_iter()
        .map(|context| SubmissionRenderSegment::Context {
            payload: PrivateHistoryPayload::Present { value: context },
        })
        .collect::<Vec<_>>();
    for (position, entry) in entries.iter().enumerate() {
        if let PendingInput::System {
            observation_id,
            observation,
            ..
        } = entry
        {
            segments.push(SubmissionRenderSegment::Input {
                input_position: u32::try_from(position)
                    .map_err(|_| CalmError::Conflict("input position is too large"))?,
                input: SubmissionRenderInput::System {
                    observation_id: observation_id.clone(),
                    payload: PrivateHistoryPayload::Present {
                        value: observation.clone(),
                    },
                },
            });
            continue;
        }
        let PendingInput::Human { message, .. } = entry else {
            return Err(CalmError::Conflict(
                "expected human input after system observation",
            ));
        };
        let identity = super::message_tx(tx, &message.message_id).await?;
        let MessageAuthor::AuthenticatedHuman { registered_user_id } = identity.author;
        let display_name: Option<String> =
            sqlx::query_scalar("SELECT display_name FROM application_users WHERE id=?1")
                .bind(&registered_user_id)
                .fetch_optional(&mut **tx)
                .await?;
        let display_name = display_name.ok_or(CalmError::Conflict(
            "human input author is no longer registered",
        ))?;
        segments.push(SubmissionRenderSegment::Input {
            input_position: u32::try_from(position)
                .map_err(|_| CalmError::Conflict("human input position is too large"))?,
            input: SubmissionRenderInput::Human {
                message: message.clone(),
                author: FrozenHumanAttribution {
                    registered_user_id,
                    display_name,
                },
            },
        });
    }
    Ok(SubmissionRenderPlan::V1 { segments })
}

/// Human-specific ingress keeps rejecting another source before building its plan.
pub async fn human_plan_for_pending_prefix_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &super::CurrentInputCarrier,
    input_count: NonZeroU32,
    contexts: Vec<SystemRenderContext>,
) -> Result<SubmissionRenderPlan> {
    if !carrier
        .checkpoint()
        .pending
        .iter()
        .take(input_count.get() as usize)
        .all(|input| matches!(input, PendingInput::Human { .. }))
    {
        return Err(CalmError::Conflict(
            "human plan cannot reinterpret another pending source",
        ));
    }
    pending_plan_for_prefix_tx(tx, carrier, input_count, contexts).await
}

/// Identity and execution provenance come from their original owners, never a
/// render label, a current author, or an unrelated runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSubmissionIdentity {
    pub card_id: String,
    pub logical_turn_id: LogicalTurnId,
    pub ordinal: u32,
    pub execution: ConversationExecutionProvenance,
}

/// Body-free identity of the original invocation, not permission or native END
/// evidence. A settled report is distinct from the pre-call crossing marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginalInputExecution {
    pub identity: StoredSubmissionIdentity,
    pub proof: SubmissionDispatchProof,
    pub correlation: InputCorrelationFact,
}

impl OriginalInputExecution {
    pub fn turn_id(&self) -> Option<&str> {
        let correlation = &self.correlation;
        if correlation.thread_id.is_empty() {
            return None;
        }
        match (
            correlation.native_turn_id.as_deref(),
            correlation.native_session_id.as_deref(),
            correlation.local_turn_id.as_deref(),
        ) {
            (Some(turn), _, None)
                if !turn.is_empty()
                    && correlation.provider.protocol()
                        == calm_types::runtime::AgentProtocol::Codex =>
            {
                Some(turn)
            }
            (None, session, Some(turn))
                if correlation.provider.protocol()
                    == calm_types::runtime::AgentProtocol::Claude
                    && !turn.is_empty()
                    && session.is_none_or(|session| !session.is_empty()) =>
            {
                Some(turn)
            }
            (Some(_), Some(_) | None, Some(_))
            | (Some(_), Some(_) | None, None)
            | (None, Some(_) | None, Some(_) | None) => None,
        }
    }

    fn same_execution(&self, other: &InputCorrelationFact) -> bool {
        let original = &self.correlation;
        original.provider == other.provider
            && original.thread_id == other.thread_id
            && original.native_turn_id == other.native_turn_id
            && original.local_turn_id == other.local_turn_id
            && match (&original.native_session_id, &other.native_session_id) {
                (Some(a), Some(b)) => a == b,
                (None, None) => true,
                // A later native init may refine the exact local invocation.
                (None, Some(session)) | (Some(session), None) => !session.is_empty(),
            }
    }
}

/// Follows the actual Turn->Initial back-reference. Missing/unsettled reports
/// hold the complete active logical execution so its real report/release owner
/// can still settle. Array order, latest native slots and text are not evidence.
pub async fn original_input_execution_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &super::CurrentInputCarrier,
) -> Result<Option<OriginalInputExecution>> {
    let Some(active) = carrier.checkpoint().active.as_ref() else {
        return Ok(None);
    };
    let original: Option<(String, String)> = sqlx::query_as(
        "SELECT t.initial_submission_id,t.initiating_runtime_id FROM conversation_turns t
         JOIN conversation_input_submissions s ON s.id=t.initial_submission_id
           AND s.card_id=t.card_id AND s.logical_turn_id=t.id AND s.kind='initial' AND s.ordinal=0
         WHERE t.id=?1 AND t.card_id=?2",
    )
    .bind(active.logical_turn_id.as_str())
    .bind(carrier.card_id())
    .fetch_optional(&mut **tx)
    .await?;
    let (original_id, initiating_runtime) = original
        .ok_or_else(|| CalmError::Conflict("active logical execution has no own Initial"))?;
    if initiating_runtime != carrier.worker_session_id() {
        return Ok(None);
    }
    let original_id = InputSubmissionId::parse(&original_id)
        .map_err(|error| CalmError::Internal(error.to_string()))?;
    if !active.submission_ids.contains(&original_id) {
        return Err(CalmError::Conflict(
            "active execution lost its Original Initial",
        ));
    }
    let Some(initial) = carrier.checkpoint().attempt(&original_id) else {
        return Ok(None);
    };
    if !matches!(
        initial.delivery,
        InputDeliveryFact::Unknown | InputDeliveryFact::Accepted { .. }
    ) {
        return Ok(None);
    }
    let stored = submission_plan_tx(tx, &original_id).await?;
    // This is reconstruction of existing proof identity, including after payload
    // erasure. The fresh-dispatch constructor would rehash tombstones instead.
    let proof = SubmissionDispatchProof::V2 {
        attempt: initial.attempt.clone(),
        input_refs: stored.plan.input_refs().map_err(CalmError::Conflict)?,
        render_plan: SubmissionRenderPlanRef {
            version: match stored.plan {
                SubmissionRenderPlan::V1 { .. } => SubmissionRenderPlanVersion::V1,
            },
            submission_id: original_id.clone(),
            sha256: stored.original_digest,
        },
    };
    let original = OriginalInputExecution {
        identity: submission_identity_tx(tx, &original_id).await?,
        proof,
        correlation: initial.correlation.clone(),
    };
    if original.turn_id().is_none() {
        return Ok(None);
    }
    for submission in &active.submission_ids {
        let Some(report) = carrier.checkpoint().attempt(submission) else {
            return Ok(None);
        };
        if !matches!(
            report.delivery,
            InputDeliveryFact::Unknown | InputDeliveryFact::Accepted { .. }
        ) || !original.same_execution(&report.correlation)
        {
            return Ok(None);
        }
        let identity = submission_identity_tx(tx, submission).await?;
        if identity.card_id != carrier.card_id()
            || identity.logical_turn_id != active.logical_turn_id
            || ((submission == &original_id) != (identity.ordinal == 0))
        {
            return Err(CalmError::Conflict(
                "active submission crosses its original execution",
            ));
        }
    }
    Ok(Some(original))
}

pub async fn submission_identity_tx(
    tx: &mut Transaction<'_, Sqlite>,
    submission_id: &InputSubmissionId,
) -> Result<StoredSubmissionIdentity> {
    let row = sqlx::query(
        "SELECT s.card_id,s.logical_turn_id,s.ordinal,t.execution_kind,
                t.deployment_owner_user_id,t.provider,t.profile
         FROM conversation_input_submissions s
         JOIN conversation_turns t ON t.id=s.logical_turn_id AND t.card_id=s.card_id
         WHERE s.id=?1",
    )
    .bind(submission_id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| CalmError::NotFound(format!("submission {submission_id}")))?;
    if row.try_get::<&str, _>("execution_kind")? != "legacy_single_user_deployment" {
        return Err(CalmError::Conflict(
            "unsupported submission execution provenance",
        ));
    }
    let provider = serde_json::from_value(serde_json::Value::String(row.try_get("provider")?))?;
    let profile = serde_json::from_value(serde_json::Value::String(row.try_get("profile")?))?;
    Ok(StoredSubmissionIdentity {
        card_id: row.try_get("card_id")?,
        logical_turn_id: LogicalTurnId::parse(row.try_get("logical_turn_id")?)
            .map_err(|error| CalmError::Internal(error.to_string()))?,
        ordinal: u32::try_from(row.try_get::<i64, _>("ordinal")?)
            .map_err(|_| CalmError::Internal("invalid submission ordinal".into()))?,
        execution: ConversationExecutionProvenance::LegacySingleUserDeployment {
            deployment_owner_user_id: row.try_get("deployment_owner_user_id")?,
            provider,
            profile,
        },
    })
}

/// Reads the immutable refs and original digest. A payload erased after claim is
/// never rehashed into a replacement proof, or loaded from an old envelope.
pub async fn submission_plan_tx(
    tx: &mut Transaction<'_, Sqlite>,
    submission_id: &InputSubmissionId,
) -> Result<StoredSubmissionPlan> {
    let stored: Option<(String,String)> = sqlx::query_as(
        "SELECT render_plan_json,render_plan_sha256 FROM conversation_resolved_submission_plans WHERE id=?1",
    ).bind(submission_id.as_str()).fetch_optional(&mut **tx).await?;
    let (plan_json, original_digest) =
        stored.ok_or_else(|| CalmError::NotFound(format!("submission {submission_id}")))?;
    let plan: SubmissionRenderPlan = serde_json::from_str(&plan_json)?;
    let original_digest = SubmissionRenderDigest::parse(&original_digest)
        .map_err(|error| CalmError::Internal(error.to_string()))?;
    let plan_refs = plan
        .input_refs()
        .map_err(|error| CalmError::Internal(error.into()))?;
    let rows = sqlx::query(
        "SELECT input_position,input_kind,message_id,revision,observation_id
         FROM conversation_submission_inputs
         WHERE submission_id=?1 ORDER BY input_position",
    )
    .bind(submission_id.as_str())
    .fetch_all(&mut **tx)
    .await?;
    let mut stored_refs = Vec::with_capacity(rows.len());
    for (position, row) in rows.iter().enumerate() {
        if row.try_get::<i64, _>("input_position")? != position as i64 {
            return Err(CalmError::Internal(
                "submission claim positions are not contiguous".into(),
            ));
        }
        let kind = row.try_get::<&str, _>("input_kind")?;
        let reference = match kind {
            "human" => {
                let message_id = ConversationMessageId::parse(row.try_get("message_id")?)
                    .map_err(|error| CalmError::Internal(error.to_string()))?;
                let revision = u32::try_from(row.try_get::<i64, _>("revision")?)
                    .ok()
                    .and_then(NonZeroU32::new)
                    .ok_or_else(|| CalmError::Internal("invalid submitted revision".into()))?;
                let message = MessageRevisionRef {
                    message_id,
                    revision,
                };
                AcceptedInputRef::Human { message }
            }
            "system" => AcceptedInputRef::System {
                observation_id: SystemObservationId::parse(row.try_get("observation_id")?)
                    .map_err(|error| CalmError::Internal(error.to_string()))?,
            },
            _ => return Err(CalmError::Internal("invalid submitted source kind".into())),
        };
        stored_refs.push(reference);
    }
    if plan_refs != stored_refs {
        return Err(CalmError::Internal(
            "submission plan and immutable claims disagree".into(),
        ));
    }
    let erased = plan.segments().iter().any(|segment| match segment {
        SubmissionRenderSegment::Context { payload } => matches!(
            payload,
            PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. }
        ),
        SubmissionRenderSegment::Input {
            input: SubmissionRenderInput::System { payload, .. },
            ..
        } => matches!(
            payload,
            PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. }
        ),
        _ => false,
    });
    if !erased && digest(&plan)? != original_digest.as_str() {
        return Err(CalmError::Internal(
            "submission render digest mismatch".into(),
        ));
    }
    Ok(StoredSubmissionPlan {
        plan,
        original_digest,
    })
}
