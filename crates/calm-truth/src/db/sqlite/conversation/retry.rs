//! Retry intent facts and frozen text admission. No source grant is stored here.
use super::*;
use calm_types::conversation::*;
use calm_types::runtime::AgentProvider;
use sqlx::Row;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetryUnavailableReason {
    PriorInvocationNotRecorded,
    InvocationNotFullyFrozen,
    AttachmentsNotFrozen,
    ErasedOrWithdrawn,
    UnsupportedBinding,
    UnsupportedSource,
    AlreadyResolved,
    NotHeld,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredRetryIntent {
    pub pending: PendingInput,
    pub predecessor_submission_id: InputSubmissionId,
    pub root_submission_id: InputSubmissionId,
    pub principal: String,
    pub plan: StoredSubmissionPlan,
    pub invocation: FrozenInputInvocation,
    pub unavailable: Option<RetryUnavailableReason>,
}

/// Text and invocation policy is shared by resolution and every repeat crossing.
pub(in crate::db::sqlite) async fn retry_availability_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &CurrentInputCarrier,
    predecessor: &InputSubmissionId,
) -> Result<
    std::result::Result<(StoredSubmissionPlan, FrozenInputInvocation), RetryUnavailableReason>,
> {
    if history_contract_tx(tx, carrier.card_id()).await? != ConversationHistoryContract::Private {
        return Ok(Err(RetryUnavailableReason::UnsupportedSource));
    }
    let attempt = carrier
        .checkpoint()
        .attempt(predecessor)
        .ok_or(CalmError::Conflict("retry predecessor has no attempt"))?;
    let invocation = match &attempt.invocation {
        InputInvocation::PriorInvocationNotRecorded => {
            return Ok(Err(RetryUnavailableReason::PriorInvocationNotRecorded));
        }
        InputInvocation::Recorded { configuration } => configuration.clone(),
    };
    if invocation.model.as_ref().is_none_or(String::is_empty)
        || invocation.effort.as_ref().is_none_or(String::is_empty)
    {
        return Ok(Err(RetryUnavailableReason::InvocationNotFullyFrozen));
    }
    let (provider, thread): (String, Option<String>) =
        sqlx::query_as("SELECT provider,thread_id FROM worker_sessions WHERE id=?1 AND card_id=?2")
            .bind(carrier.worker_session_id())
            .bind(carrier.card_id())
            .fetch_one(&mut **tx)
            .await?;
    let original = submission_identity_tx(tx, predecessor).await?;
    let ConversationExecutionProvenance::LegacySingleUserDeployment {
        provider: original_provider,
        ..
    } = original.execution;
    if provider != "codex"
        || original_provider != AgentProvider::Codex
        || attempt.correlation.provider != AgentProvider::Codex
        || thread.as_deref() != Some(attempt.correlation.thread_id.as_str())
        || carrier
            .snapshot()
            .get("last_thread_id")
            .and_then(serde_json::Value::as_str)
            != thread.as_deref()
    {
        return Ok(Err(RetryUnavailableReason::UnsupportedBinding));
    }
    let stored = submission_plan_tx(tx, predecessor).await?;
    if let Some(reason) = text_unavailable_tx(tx, carrier.card_id(), &stored.plan).await? {
        return Ok(Err(reason));
    }
    Ok(Ok((stored, invocation)))
}

async fn text_unavailable_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    plan: &SubmissionRenderPlan,
) -> Result<Option<RetryUnavailableReason>> {
    for segment in plan.segments() {
        match segment {
            SubmissionRenderSegment::Input {
                input: SubmissionRenderInput::Human { message, author },
                ..
            } => {
                let identity = message_tx(tx, &message.message_id).await?;
                if identity.card_id != card
                    || identity.withdrawn_at_ms.is_some()
                    || identity.author
                        != (MessageAuthor::AuthenticatedHuman {
                            registered_user_id: author.registered_user_id.clone(),
                        })
                {
                    return Ok(Some(RetryUnavailableReason::ErasedOrWithdrawn));
                }
                match revision_tx(tx, message).await?.payload {
                    PrivateHistoryPayload::Present { value } => {
                        if !value.bound_attachments.is_empty() {
                            return Ok(Some(RetryUnavailableReason::AttachmentsNotFrozen));
                        }
                    }
                    PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. } => {
                        return Ok(Some(RetryUnavailableReason::ErasedOrWithdrawn));
                    }
                }
            }
            SubmissionRenderSegment::Input {
                input: SubmissionRenderInput::System { payload, .. },
                ..
            } => match payload {
                PrivateHistoryPayload::Present { .. } => {}
                PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. } => {
                    return Ok(Some(RetryUnavailableReason::ErasedOrWithdrawn));
                }
            },
            SubmissionRenderSegment::Context { payload } => match payload {
                PrivateHistoryPayload::Present { .. } => {}
                PrivateHistoryPayload::ErasedByPrivateHistoryOperation { .. } => {
                    return Ok(Some(RetryUnavailableReason::ErasedOrWithdrawn));
                }
            },
        }
    }
    Ok(None)
}

/// Verify the relation against the sole checkpoint receipt on every live use.
pub async fn retry_intent_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &CurrentInputCarrier,
    id: &RetryIntentId,
) -> Result<StoredRetryIntent> {
    let row = sqlx::query("SELECT * FROM conversation_input_retry_relations WHERE retry_intent_id=?1 AND card_id=?2 AND carrier_id=?3")
        .bind(id.as_str()).bind(carrier.card_id()).bind(carrier.worker_session_id()).fetch_optional(&mut **tx).await?
        .ok_or(CalmError::Conflict("retry intent does not belong to this carrier"))?;
    let predecessor = InputSubmissionId::parse(row.try_get("predecessor_submission_id")?)
        .map_err(|e| CalmError::Internal(e.to_string()))?;
    let attempt: String = row.try_get("predecessor_attempt_id")?;
    let key: String = row.try_get("resolution_key")?;
    let principal: String = row.try_get("principal_user_id")?;
    let fingerprint: String = row.try_get("fingerprint")?;
    if !carrier.checkpoint().receipts.iter().any(|r| {
        r.key == key
            && r.principal == principal
            && r.fingerprint == fingerprint
            && match &r.decision {
                HumanInputResolutionDecision::Keep { .. } => false,
                HumanInputResolutionDecision::Retry {
                    predecessor: p,
                    retry_intent_id,
                    ..
                } => {
                    retry_intent_id == id
                        && p.submission_id == predecessor.as_str()
                        && p.attempt_id == attempt
                        && p.carrier_id == carrier.worker_session_id()
                }
            }
    }) {
        return Err(CalmError::Conflict(
            "retry intent lost its exact resolution receipt",
        ));
    }
    let unavailable = retry_availability_tx(tx, carrier, &predecessor)
        .await?
        .err();
    let plan = submission_plan_tx(tx, &predecessor).await?;
    let invocation = match &carrier
        .checkpoint()
        .attempt(&predecessor)
        .ok_or(CalmError::Conflict("retry predecessor attempt missing"))?
        .invocation
    {
        InputInvocation::Recorded { configuration } => configuration.clone(),
        InputInvocation::PriorInvocationNotRecorded => {
            return Err(CalmError::Conflict(
                "retry intent has no original invocation",
            ));
        }
    };
    let root = InputSubmissionId::parse(row.try_get("root_submission_id")?)
        .map_err(|e| CalmError::Internal(e.to_string()))?;
    let actual_root: String = sqlx::query_scalar("SELECT COALESCE(render_plan_root_submission_id,id) FROM conversation_input_submissions WHERE id=?1 AND card_id=?2")
        .bind(predecessor.as_str()).bind(carrier.card_id()).fetch_one(&mut **tx).await?;
    if root.as_str() != actual_root {
        return Err(CalmError::Conflict("retry root changed"));
    }
    Ok(StoredRetryIntent {
        pending: PendingInput::ExplicitSuccessor {
            retry_intent_id: id.clone(),
            queue_position: u64::try_from(row.try_get::<i64, _>("queue_position")?)
                .map_err(|_| CalmError::Conflict("invalid retry queue position"))?,
            queued_at_ms: row.try_get("created_at_ms")?,
        },
        predecessor_submission_id: predecessor,
        root_submission_id: root,
        principal,
        plan,
        invocation,
        unavailable,
    })
}

/// A crossing carries its intent identity independently of the original input refs.
pub async fn submission_retry_intent_tx(
    tx: &mut Transaction<'_, Sqlite>,
    id: &InputSubmissionId,
) -> Result<Option<RetryIntentId>> {
    let value: Option<String> = sqlx::query_scalar(
        "SELECT retry_intent_id FROM conversation_input_submissions WHERE id=?1",
    )
    .bind(id.as_str())
    .fetch_one(&mut **tx)
    .await?;
    value
        .map(|value| RetryIntentId::parse(&value).map_err(|e| CalmError::Internal(e.to_string())))
        .transpose()
}

pub(super) fn held_unavailable(
    carrier: &CurrentInputCarrier,
    index: usize,
) -> Option<RetryUnavailableReason> {
    let attempt = &carrier.checkpoint().attempts[index];
    if carrier.checkpoint().is_human_resolved(&attempt.attempt) {
        Some(RetryUnavailableReason::AlreadyResolved)
    } else if !matches!(
        attempt.delivery,
        InputDeliveryFact::CrossingUnknown | InputDeliveryFact::Unknown
    ) {
        Some(RetryUnavailableReason::NotHeld)
    } else {
        None
    }
}

pub(super) async fn resolve_retry_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    human: &str,
    key: &str,
    request: &KeepConversationInputRequest,
    current_execution: &ConversationExecutionProvenance,
) -> Result<std::result::Result<HumanInputResolutionDecision, RetryUnavailableReason>> {
    let fingerprint = digest(&("human_retry_v1", card, human, request))?;
    if let Some(decision) = replay_retry_tx(tx, card, human, key, request).await? {
        return Ok(Ok(decision));
    }
    let (mut carrier, index) = super::resolution::resolution_target_tx(tx, card, request).await?;
    if let Some(reason) = held_unavailable(&carrier, index) {
        return Ok(Err(reason));
    }
    let predecessor = carrier.checkpoint().attempts[index]
        .attempt
        .submission_id
        .clone();
    if submission_identity_tx(tx, &predecessor).await?.execution != *current_execution {
        return Ok(Err(RetryUnavailableReason::UnsupportedBinding));
    }
    let attempt = &carrier.checkpoint().attempts[index];
    let revision = attempt
        .revision
        .checked_add(1)
        .ok_or(CalmError::Conflict("input attempt revision exhausted"))?;
    let waiting_for_settlement = attempt.correlation.native_turn_id.is_none();
    if let Err(reason) = retry_availability_tx(tx, &carrier, &predecessor).await? {
        return Ok(Err(reason));
    }
    let retry_intent_id = RetryIntentId::parse(&crate::model::new_id())
        .map_err(|e| CalmError::Internal(e.to_string()))?;
    let (queue_position, next) = super::acceptance::pending_position(&carrier)?;
    let position = i64::try_from(queue_position)
        .map_err(|_| CalmError::Conflict("pending input order exceeds storage range"))?;
    let created_at_ms = crate::model::now_ms();
    let decision = HumanInputResolutionDecision::Retry {
        predecessor: request.clone(),
        retry_intent_id: retry_intent_id.clone(),
        revision: revision.to_string(),
        waiting_for_settlement,
    };
    carrier
        .checkpoint
        .attempts
        .iter_mut()
        .find(|a| a.attempt.submission_id == predecessor)
        .expect("selected predecessor")
        .revision = revision;
    carrier
        .checkpoint
        .receipts
        .push(HumanInputResolutionReceipt {
            key: key.into(),
            principal: human.into(),
            fingerprint: fingerprint.clone(),
            decision: decision.clone(),
        });
    carrier
        .checkpoint
        .pending
        .push(PendingInput::ExplicitSuccessor {
            retry_intent_id: retry_intent_id.clone(),
            queue_position,
            queued_at_ms: created_at_ms,
        });
    carrier.checkpoint.next_queue_position = next;
    super::carrier::save_input_carrier_tx(tx, &mut carrier).await?;
    sqlx::query("INSERT INTO conversation_input_retry_relations(retry_intent_id,card_id,carrier_id,
        predecessor_submission_id,root_submission_id,predecessor_attempt_id,resolution_key,
        principal_user_id,fingerprint,queue_position,created_at_ms)
        SELECT ?1,?2,?3,id,COALESCE(render_plan_root_submission_id,id),?5,?6,?7,?8,?9,?10 FROM conversation_input_submissions WHERE id=?4 AND card_id=?2")
        .bind(retry_intent_id.as_str()).bind(card).bind(&request.carrier_id).bind(&request.submission_id).bind(&request.attempt_id).bind(key).bind(human).bind(fingerprint).bind(position).bind(created_at_ms).execute(&mut **tx).await?;
    let expected: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversation_submission_inputs WHERE submission_id=?1",
    )
    .bind(&request.submission_id)
    .fetch_one(&mut **tx)
    .await?;
    let retired = sqlx::query("UPDATE conversation_submission_inputs SET effective=0,retirement_reason='human_resolution',
        retired_by_retry_intent_id=?1 WHERE submission_id=?2 AND effective=1")
        .bind(retry_intent_id.as_str()).bind(&request.submission_id).execute(&mut **tx).await?;
    if expected == 0 || retired.rows_affected() != expected as u64 {
        return Err(CalmError::Conflict("retry predecessor claims changed"));
    }
    Ok(Ok(decision))
}

/// Same shared receipt lookup, before any current execution qualification.
pub(super) async fn replay_retry_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    human: &str,
    key: &str,
    request: &KeepConversationInputRequest,
) -> Result<Option<HumanInputResolutionDecision>> {
    let fingerprint = digest(&("human_retry_v1", card, human, request))?;
    super::resolution::replay_tx(tx, card, human, key, &fingerprint).await
}
