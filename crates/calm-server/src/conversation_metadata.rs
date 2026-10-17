//! Server-owned conversation metadata: first visible persisted input, then the projectable pending queue.
use std::collections::HashMap;

use crate::db::RepoRead;
use crate::error::Result;
use crate::harness::HarnessSnapshot;
use crate::harness::queue::input_segments_for_entries;
use crate::session_projection_repo::WorkerSessionProjectionRepo;
use crate::session_projection_repo::{CardId, WorkerSessionProjection};
use calm_truth::conversation_title::{ConversationTitleSource, first_user_title};

/// The metadata owner's two batch reads. Implementations delegate eligibility and source
/// selection to the existing repositories; ordering belongs to `titles`.
#[async_trait::async_trait]
pub(crate) trait TitleRead: Send + Sync {
    async fn sources(&self, ids: &[String]) -> Result<Vec<ConversationTitleSource>>;
    async fn pending(&self, ids: &[String]) -> Result<HashMap<CardId, WorkerSessionProjection>>;
}

#[async_trait::async_trait]
impl<R: RepoRead + WorkerSessionProjectionRepo + ?Sized> TitleRead for R {
    async fn sources(&self, ids: &[String]) -> Result<Vec<ConversationTitleSource>> {
        Ok(self.conversation_title_sources(ids).await?)
    }

    async fn pending(&self, ids: &[String]) -> Result<HashMap<CardId, WorkerSessionProjection>> {
        Ok(self.session_projection_projectable_for_cards(ids).await?)
    }
}

pub(crate) async fn titles<R: TitleRead + ?Sized>(
    repo: &R,
    card_ids: &[String],
) -> Result<HashMap<String, Option<String>>> {
    if card_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let sources = repo.sources(card_ids).await?;
    let pending_ids: Vec<_> = sources
        .iter()
        .filter(|source| source.input.is_none())
        .map(|source| source.card_id.clone())
        .collect();
    let runtimes = if pending_ids.is_empty() {
        HashMap::new()
    } else {
        repo.pending(&pending_ids).await?
    };
    Ok(sources
        .into_iter()
        .map(|source| {
            let title = match source.input {
                Some(input) => input.title(),
                None => runtimes
                    .get(&source.card_id)
                    .and_then(|runtime| runtime.handle_state_json.clone())
                    .and_then(HarnessSnapshot::parse_known)
                    .and_then(|snapshot| {
                        let segments = input_segments_for_entries(
                            &source.card_id.clone().into(),
                            &snapshot.pending_entries(),
                        );
                        first_user_title(&segments).flatten()
                    }),
            };
            (source.card_id, title)
        })
        .collect())
}

#[cfg(test)]
mod tests;
