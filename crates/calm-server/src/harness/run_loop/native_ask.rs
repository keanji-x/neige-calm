//! A question the provider's own model put to the user (#2209 U2) becomes the Planner's ask: the
//! same `ask.requested` `neige_user_ask` writes, so the user sees it, answers it, and the answer
//! wakes the Planner. The provider's arm has already read its wire shape into neutral questions.

use calm_types::event::AskQuestion;

use super::Inner;
use crate::db::write_with_actor_events_typed;
use crate::harness::profile::HarnessProfile;
use crate::ids::ActorId;
use crate::state::WriteContext;

/// Write the Planner's `ask.requested` for one completed item that asks the user `questions`,
/// once per item (`item_id`). Only a Planner conversation asks: a PlainChat or Assistant card runs
/// this loop too and writes nothing. A refused write (the role gate, a question the shared entry
/// does not accept, a card gone by the write) is logged and skipped: it never fails the event, so
/// the harness goes on with the next one.
pub(super) async fn ask_from_item(
    inner: &Inner,
    item_id: Option<&str>,
    questions: Vec<AskQuestion>,
) {
    if !is_planner_conversation(inner).await {
        tracing::debug!(
            card_id = %inner.card_id,
            "planner harness: not a Planner conversation; a provider question asks nothing"
        );
        return;
    }
    let Some(item_id) = item_id else {
        tracing::warn!(
            card_id = %inner.card_id,
            "planner harness: a provider question without an item id cannot be asked once; skipped"
        );
        return;
    };
    let card = inner.card_id.clone();
    let source_item_id = item_id.to_string();
    let written = write_with_actor_events_typed::<(), _>(
        inner.repo.as_ref(),
        None,
        &inner.events,
        &WriteContext::new(
            inner.card_role_cache.clone(),
            inner.track_area_cache.clone(),
        ),
        move |tx| {
            Box::pin(async move {
                let asked =
                    crate::ask::provider_ask_requested_tx(tx, &card, questions, source_item_id)
                        .await?;
                Ok((
                    (),
                    asked
                        .map(|(scope, event)| (ActorId::AiPlanner(card), scope, event))
                        .into_iter()
                        .collect(),
                ))
            })
        },
    )
    .await;
    if let Err(error) = written {
        tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            item_id,
            %error,
            "planner harness: refused to ask the user a provider question; skipped"
        );
    }
}

/// Whether this loop runs the card's Planner conversation, by the profile its card names. A card
/// that cannot be read is not one.
async fn is_planner_conversation(inner: &Inner) -> bool {
    let card_id = inner.card_id.as_str();
    let (card, role) = match (
        inner.repo.card_get(card_id).await,
        inner.repo.card_role_get(card_id).await,
    ) {
        (Ok(Some(card)), Ok(Some(role))) => (card, role),
        _ => return false,
    };
    HarnessProfile::from_shape(&card.kind, role, &card.payload) == Some(HarnessProfile::Planner)
}
