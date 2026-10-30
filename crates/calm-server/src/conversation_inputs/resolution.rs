//! Human resolution admission. No provider behavior or stored source grants.
use super::*;
use calm_types::conversation::{
    ConversationExecutionProvenance, ConversationInputResolutionResult,
    KeepConversationInputRequest, KeepConversationInputResult, RetryConversationInputRequest,
    RetryConversationInputResult,
};

pub(crate) async fn keep(
    repo: &dyn crate::db::RepoEventWrite,
    registry: crate::harness::registry::HarnessRegistry,
    source: HumanInputSource,
    card_id: String,
    key: String,
    request: KeepConversationInputRequest,
) -> Result<(
    KeepConversationInputResult,
    crate::event::BroadcastEnvelope,
    crate::harness::registry::SourceHandoff,
)> {
    let (result, event, handoff) = resolve(
        repo,
        registry,
        source,
        card_id,
        key,
        Decision::Keep(request),
    )
    .await?;
    match result {
        ConversationInputResolutionResult::Keep { result } => Ok((result, event, handoff)),
        ConversationInputResolutionResult::Retry { .. } => {
            Err(CalmError::Internal("Keep returned Retry".into()))
        }
    }
}
pub(crate) async fn retry(
    repo: &dyn crate::db::RepoEventWrite,
    registry: crate::harness::registry::HarnessRegistry,
    source: HumanInputSource,
    card_id: String,
    key: String,
    request: RetryConversationInputRequest,
) -> Result<(
    RetryConversationInputResult,
    crate::event::BroadcastEnvelope,
    crate::harness::registry::SourceHandoff,
)> {
    let (result, event, handoff) = resolve(
        repo,
        registry,
        source,
        card_id,
        key,
        Decision::Retry(request),
    )
    .await?;
    match result {
        ConversationInputResolutionResult::Retry { result } => Ok((result, event, handoff)),
        ConversationInputResolutionResult::Keep { .. } => {
            Err(CalmError::Internal("Retry returned Keep".into()))
        }
    }
}
enum Decision {
    Keep(KeepConversationInputRequest),
    Retry(RetryConversationInputRequest),
}
async fn resolve(
    repo: &dyn crate::db::RepoEventWrite,
    registry: crate::harness::registry::HarnessRegistry,
    source: HumanInputSource,
    card_id: String,
    key: String,
    request: Decision,
) -> Result<(
    ConversationInputResolutionResult,
    crate::event::BroadcastEnvelope,
    crate::harness::registry::SourceHandoff,
)> {
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let principal = source.current_principal()?;
            let area = authorize_human_tx(tx, &principal, &card_id).await?;
            super::authorize_human_resource_tx(tx, &source, &card_id).await?;
            let result = match request {
                Decision::Keep(request) => ConversationInputResolutionResult::Keep {
                    result: conversation::keep_conversation_input_tx(
                        tx,
                        &card_id,
                        &principal.user_id,
                        &key,
                        &request,
                    )
                    .await?,
                },
                Decision::Retry(request) => {
                    if let Some(result) = conversation::replay_retry_conversation_input_tx(
                        tx,
                        &card_id,
                        &principal.user_id,
                        &key,
                        &request,
                    )
                    .await?
                    {
                        ConversationInputResolutionResult::Retry { result }
                    } else {
                        let carrier = conversation::current_input_fact_carrier_tx(
                            tx,
                            &card_id,
                            &request.carrier_id,
                        )
                        .await?;
                        let (binding, _) =
                            crate::harness::profile::input_carrier_contract_tx(tx, &carrier)
                                .await?;
                        let execution =
                            ConversationExecutionProvenance::LegacySingleUserDeployment {
                                deployment_owner_user_id: principal.user_id.clone(),
                                provider: binding.provider,
                                profile: binding.profile,
                            };
                        ConversationInputResolutionResult::Retry {
                            result: conversation::retry_conversation_input_tx(
                                tx,
                                &card_id,
                                &principal.user_id,
                                &key,
                                &request,
                                &execution,
                            )
                            .await?,
                        }
                    }
                }
            };
            let carrier_id = match &result {
                ConversationInputResolutionResult::Keep { result } => &result.carrier_id,
                ConversationInputResolutionResult::Retry { result } => &result.carrier_id,
            };
            // Same-phase invalidation also reaches dormant views. Replay emits
            // refresh only; it never changes the original checkpoint decision.
            let (track, raw): (String, String) = sqlx::query_as(
                "SELECT track_id,handle_state_json FROM worker_sessions WHERE id=?1 AND card_id=?2",
            )
            .bind(carrier_id)
            .bind(&card_id)
            .fetch_one(&mut **tx)
            .await?;
            let frame: calm_types::harness::snapshot::CanonicalHarnessSnapshot =
                serde_json::from_str(&raw)?;
            let handoff = registry
                .canonical_sources(carrier_id, &track.clone().into())
                .begin_handoff();
            let scope = crate::event::EventScope::Card {
                card: card_id.clone().into(),
                track: track.clone().into(),
                area: area.id,
            };
            let event = crate::event::Event::HarnessPhaseChanged {
                worker_session_id: carrier_id.clone(),
                card_id: card_id.into(),
                track_id: track.into(),
                old_phase: frame.header.phase,
                new_phase: frame.header.phase,
            };
            let id = crate::db::sqlite::append_decision_event_in_tx(
                tx,
                &crate::ids::ActorId::User,
                &scope,
                None,
                &event,
            )
            .await?;
            source.current_principal()?;
            Ok((
                result,
                crate::event::BroadcastEnvelope {
                    id,
                    event_version: crate::event::SYNC_EVENT_VERSION,
                    actor: crate::ids::ActorId::User,
                    scope,
                    event,
                },
                handoff,
            ))
        })
    })
    .await
}

/// Supplement Truth's frozen-content facts with the same current source/binding
/// admission used by resolution. Availability never grants a future crossing.
pub(crate) async fn retry_read(
    repo: &dyn crate::db::RepoEventWrite,
    source: Option<HumanInputSource>,
    card: String,
    active: Option<calm_types::conversation_display::ConversationActiveInput>,
) -> Result<Option<calm_types::conversation_display::ConversationActiveInput>> {
    use calm_types::conversation_display::{
        ConversationActionAvailability as A, ConversationActionUnavailableReason as R,
    };
    let Some(mut active) = active else {
        return Ok(None);
    };
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            for submission in &mut active.submissions {
                let Some(attempt) = &mut submission.attempt else {
                    continue;
                };
                if attempt.actions.retry != A::Available {
                    continue;
                }
                let Some(source) = &source else {
                    attempt.actions.retry = A::Unavailable {
                        reason: R::UnsupportedSource,
                    };
                    continue;
                };
                match super::authorize_human_resource_tx(tx, source, &card).await {
                    Ok(()) => {}
                    Err(CalmError::Forbidden(_)) => {
                        attempt.actions.retry = A::Unavailable {
                            reason: R::UnsupportedSource,
                        };
                        continue;
                    }
                    Err(error) => return Err(error),
                }
                let carrier =
                    conversation::current_input_fact_carrier_tx(tx, &card, &submission.carrier_id)
                        .await?;
                let (binding, _) =
                    crate::harness::profile::input_carrier_contract_tx(tx, &carrier).await?;
                let current = ConversationExecutionProvenance::LegacySingleUserDeployment {
                    deployment_owner_user_id: source.current_principal()?.user_id,
                    provider: binding.provider,
                    profile: binding.profile,
                };
                let id =
                    calm_types::conversation::InputSubmissionId::parse(&submission.submission_id)
                        .map_err(|error| CalmError::Internal(error.to_string()))?;
                if conversation::submission_identity_tx(tx, &id)
                    .await?
                    .execution
                    != current
                {
                    attempt.actions.retry = A::Unavailable {
                        reason: R::UnsupportedBinding,
                    };
                }
                source.current_principal()?;
            }
            Ok(Some(active))
        })
    })
    .await
}
