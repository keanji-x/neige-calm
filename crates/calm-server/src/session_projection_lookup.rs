//! Server API facade combines generic runtime identity with conversation-owned metadata.
pub use calm_truth::session_projection_lookup::*;

use crate::db::RepoRead;
use crate::error::Result;
use crate::event::Event;
use crate::model::Card;
use crate::session_projection_repo::WorkerSessionProjectionRepo;

pub async fn project_runtime_into_card_payload<
    R: RepoRead + WorkerSessionProjectionRepo + ?Sized,
>(
    repo: &R,
    card: &mut Card,
) -> Result<()> {
    project_runtime_into_cards_payload(repo, std::slice::from_mut(card)).await
}

pub async fn project_runtime_into_cards_payload<
    R: RepoRead + WorkerSessionProjectionRepo + ?Sized,
>(
    repo: &R,
    cards: &mut [Card],
) -> Result<()> {
    calm_truth::session_projection_lookup::project_runtime_into_cards_payload(repo, cards).await?;
    let ids = cards
        .iter()
        .filter(|card| card.title.is_none())
        .map(|card| card.id.to_string())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(());
    }
    let mut titles = crate::conversation_metadata::titles(repo, &ids).await?;
    for card in cards {
        if card.title.is_none() {
            card.title = titles.remove(card.id.as_str()).flatten();
        }
    }
    Ok(())
}

pub async fn project_runtime_into_event_payload<
    R: RepoRead + WorkerSessionProjectionRepo + ?Sized,
>(
    repo: &R,
    event: &mut Event,
) -> Result<()> {
    match event {
        Event::CardAdded(card) | Event::CardUpdated(card) => {
            project_runtime_into_card_payload(repo, card).await?
        }
        _ => {}
    }
    Ok(())
}
