//! Server-owned conversation metadata: first visible persisted input, then the projectable pending queue.
use std::collections::HashMap;

use crate::db::RepoRead;
use crate::error::Result;
use crate::harness::HarnessSnapshot;
use crate::harness::queue::input_segments_for_entries;
use crate::session_projection_repo::WorkerSessionProjectionRepo;
use calm_truth::conversation_title::first_user_title;

pub(crate) async fn titles<R: RepoRead + WorkerSessionProjectionRepo + ?Sized>(
    repo: &R,
    card_ids: &[String],
) -> Result<HashMap<String, Option<String>>> {
    if card_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let sources = repo.conversation_title_sources(card_ids).await?;
    let pending_ids: Vec<_> = sources
        .iter()
        .filter(|source| source.input.is_none())
        .map(|source| source.card_id.clone())
        .collect();
    let runtimes = if pending_ids.is_empty() {
        HashMap::new()
    } else {
        repo.session_projection_projectable_for_cards(&pending_ids)
            .await?
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
