//! Human resolution admission. No provider behavior or stored source grants.
use super::*;
use calm_types::conversation::{KeepConversationInputRequest, KeepConversationInputResult};

pub(crate) async fn keep(
    repo: &dyn crate::db::RepoEventWrite,
    source: HumanInputSource,
    card_id: String,
    key: String,
    request: KeepConversationInputRequest,
) -> Result<(KeepConversationInputResult, crate::event::BroadcastEnvelope)> {
    crate::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let principal = source.current_principal()?;
            let area = authorize_human_tx(tx, &principal, &card_id).await?;
            super::authorize_human_resource_tx(tx, &source, &card_id).await?;
            let result = conversation::keep_conversation_input_tx(
                tx,
                &card_id,
                &principal.user_id,
                &key,
                &request,
            )
            .await?;
            // Same-phase invalidation also reaches dormant views. Replay emits
            // refresh only; it never changes the original checkpoint decision.
            let (track, raw): (String, String) = sqlx::query_as(
                "SELECT track_id,handle_state_json FROM worker_sessions WHERE id=?1 AND card_id=?2",
            )
            .bind(&result.carrier_id)
            .bind(&card_id)
            .fetch_one(&mut **tx)
            .await?;
            let frame: calm_types::harness::snapshot::CanonicalHarnessSnapshot =
                serde_json::from_str(&raw)?;
            let scope = crate::event::EventScope::Card {
                card: card_id.clone().into(),
                track: track.clone().into(),
                area: area.id,
            };
            let event = crate::event::Event::HarnessPhaseChanged {
                worker_session_id: result.carrier_id.clone(),
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
            ))
        })
    })
    .await
}
