//! Server-owned conversation metadata: first visible persisted input, then the projectable pending queue.
use std::collections::HashMap;

use crate::db::RepoRead;
use crate::error::Result;
use crate::harness::HarnessSnapshot;
use crate::harness::queue::input_segments_for_entries;
use calm_truth::conversation_title::first_user_title;

pub(crate) async fn titles<R: RepoRead + ?Sized>(
    repo: &R,
    card_ids: &[String],
) -> Result<HashMap<String, Option<String>>> {
    if card_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let context = repo.conversation_title_context(card_ids).await?;
    let sources = context.sources;
    let runtimes = context.runtimes;
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
