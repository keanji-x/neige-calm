//! A question the provider's own model put to the user (#2209 U2) becomes the Planner's ask: the
//! same `ask.requested` `neige_user_ask` writes, so the user sees it, answers it, and the answer
//! wakes the Planner. The provider's arm has already read its wire shape into neutral questions.

use calm_types::event::AskQuestion;
use calm_types::worker::WorkerSessionId;

use super::Inner;
use crate::db::write_with_actor_events_typed;
use crate::event::Event;
use crate::harness::profile::HarnessProfile;
use crate::ids::ActorId;
use crate::state::WriteContext;

/// Write the Planner's `ask.requested` for one completed item that asks the user `questions`,
/// once per item (`item_id`). Only a Planner conversation asks: a PlainChat or Assistant card runs
/// this loop too and writes nothing. The author is this loop's session, as `neige_user_ask`'s is,
/// so the role gate refuses a session that is no longer the card's live one (a reset superseded
/// it before stopping this loop). A refused write (that, a question the shared entry does not
/// accept, a card gone by the write) is logged and skipped: it never fails the event, so the
/// harness goes on with the next one. An item already asked is not a refusal.
pub(super) async fn ask_from_item(
    inner: &Inner,
    item_id: Option<&str>,
    questions: Vec<AskQuestion>,
) {
    if !is_planner_conversation(inner).await {
        return;
    }
    let Some(item_id) = item_id else {
        tracing::warn!(
            card_id = %inner.card_id,
            "planner harness: a provider question without an item id cannot be asked once; skipped"
        );
        return;
    };
    match already_asked(inner, item_id).await {
        Ok(false) => {}
        Ok(true) => {
            tracing::debug!(
                card_id = %inner.card_id,
                item_id,
                "planner harness: this provider question is already asked"
            );
            return;
        }
        Err(error) => {
            tracing::warn!(
                card_id = %inner.card_id,
                item_id,
                %error,
                "planner harness: cannot read whether a provider question is already asked; skipped"
            );
            return;
        }
    }
    let card = inner.card_id.clone();
    let actor = ActorId::AiPlannerSession(WorkerSessionId::from(inner.worker_session_id.as_str()));
    let source_item_id = item_id.to_string();
    // The transaction checks again, so two writers racing on one item still ask once; the loser
    // is logged as refused.
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
                        .map(|(scope, event)| (actor, scope, event))
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

/// Whether the track already has the ask from `item_id`: a duplicate frame, a replay or a restart.
async fn already_asked(inner: &Inner, item_id: &str) -> crate::error::Result<bool> {
    Ok(inner
        .repo
        .events_for_track(inner.track_id.as_str(), &["ask.requested"], None)
        .await?
        .iter()
        .any(|row| {
            matches!(&row.event, Event::AskRequested { source_item_id: Some(source), .. }
                if source == item_id)
        }))
}

/// Whether this loop runs the card's Planner conversation, by the profile its card names. A card
/// that is gone is not one; a card that cannot be read is not one either, and says so.
async fn is_planner_conversation(inner: &Inner) -> bool {
    let card_id = inner.card_id.as_str();
    let read = async {
        Ok::<_, crate::error::CalmError>((
            inner.repo.card_get(card_id).await?,
            inner.repo.card_role_get(card_id).await?,
        ))
    }
    .await;
    match read {
        Ok((Some(card), Some(role))) => {
            HarnessProfile::from_shape(&card.kind, role, &card.payload)
                == Some(HarnessProfile::Planner)
        }
        Ok(_) => false,
        Err(error) => {
            tracing::warn!(
                card_id = %inner.card_id,
                %error,
                "planner harness: cannot read the card; a provider question asks nothing"
            );
            false
        }
    }
}
