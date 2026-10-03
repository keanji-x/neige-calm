//! Which transcript rows a rewind removes and what goes back into the composer (#1923). Pure over
//! the thread's rows, so every refusal is decided before anything changes.

use std::collections::HashSet;

use serde_json::Value;

use crate::db::TranscriptRow;
use crate::model::{HarnessInputPresentation, HarnessInputSegment};
use crate::planner_attachments::bind::MAX_ATTACHMENTS_PER_MESSAGE;

/// The rows of one turn, cut from the thread.
#[derive(Debug, Clone)]
pub(crate) struct RewindPlan {
    /// The highest row id that stays: every row of the thread above it belongs to the turn.
    pub boundary: i64,
    /// The turn before it, which becomes the conversation's last; `None` when it was the first.
    pub previous_turn_id: Option<String>,
    /// The previous turn's rows, for a provider that cuts its session at that turn's end.
    pub previous_turn_rows: Vec<TranscriptRow>,
    /// The client id of the turn's first user message.
    pub prompt_client_id: Option<String>,
    /// The turn's user input, prompt then accepted steers in row order, each attachment once.
    pub input: Vec<HarnessInputSegment>,
}

/// Cut turn `turn_id` from `rows` (one thread's rows, oldest first). The boundary is the highest
/// row tagged with another turn; everything above it is the turn's, its untagged projection
/// included. A row tagged with the turn at or below the boundary means an older turn's row landed
/// inside it, so the suffix would not hold the whole turn: refused.
pub(crate) fn plan(rows: &[TranscriptRow], turn_id: &str) -> Result<RewindPlan, String> {
    if !rows
        .iter()
        .any(|row| row.turn_id.as_deref() == Some(turn_id))
    {
        return Err("this turn has no record in the conversation".into());
    }
    let boundary_row = rows
        .iter()
        .filter(|row| row.turn_id.as_deref().is_some_and(|turn| turn != turn_id))
        .max_by_key(|row| row.id);
    let boundary = boundary_row.map_or(0, |row| row.id);
    if rows
        .iter()
        .any(|row| row.turn_id.as_deref() == Some(turn_id) && row.id <= boundary)
    {
        return Err(
            "a reply from an earlier turn was recorded inside this turn, so this turn cannot be \
             separated from it"
                .into(),
        );
    }
    let previous_turn_id = boundary_row.and_then(|row| row.turn_id.clone());
    let previous_turn_rows = rows
        .iter()
        .filter(|row| previous_turn_id.is_some() && row.turn_id == previous_turn_id)
        .cloned()
        .collect();
    let user_rows = rows
        .iter()
        .filter(|row| row.id > boundary)
        .filter(|row| {
            matches!(
                row.item_type.as_deref(),
                Some("userMessage" | "user_message")
            )
        })
        .collect::<Vec<_>>();
    let prompt_client_id = user_rows.first().and_then(|row| client_id(&row.params));
    let mut input = Vec::new();
    for row in user_rows {
        let Some(segments) = row
            .input_segments
            .as_deref()
            .and_then(|json| serde_json::from_str::<Vec<HarnessInputSegment>>(json).ok())
        else {
            return Err(
                "a message in this turn was recorded without its text, so it cannot be put back"
                    .into(),
            );
        };
        if segments
            .iter()
            .any(|segment| segment.presentation != HarnessInputPresentation::User)
        {
            return Err(
                "this turn also carried a system update, not only your messages, so it cannot be \
                 edited"
                    .into(),
            );
        }
        input.extend(segments);
    }
    if input.is_empty() {
        return Err("this turn holds no message of yours to edit".into());
    }
    // A steer may re-send an image the prompt already bound; the refill names it once.
    let mut seen = HashSet::new();
    for segment in &mut input {
        segment
            .attachments
            .retain(|attachment| seen.insert(attachment.id.clone()));
    }
    if seen.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        return Err(format!(
            "this turn carried {} images; one message can carry at most \
             {MAX_ATTACHMENTS_PER_MESSAGE}, so it cannot be put back as one",
            seen.len()
        ));
    }
    Ok(RewindPlan {
        boundary,
        previous_turn_id,
        previous_turn_rows,
        prompt_client_id,
        input,
    })
}

/// `params.item.clientId` of a user-message row: the projection key the kernel wrote it under.
fn client_id(params: &str) -> Option<String> {
    let params: Value = serde_json::from_str(params).ok()?;
    params
        .get("item")?
        .get("clientId")?
        .as_str()
        .map(str::to_string)
}
