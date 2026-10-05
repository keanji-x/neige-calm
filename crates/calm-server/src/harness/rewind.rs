//! Which transcript rows a replace removes (#1923, #2043). Pure over the thread's rows, so every
//! refusal is decided before anything changes.

use serde_json::Value;

use crate::db::TranscriptRow;
use crate::model::HarnessInputPresentation;

/// The rows of one turn, cut from the thread.
#[derive(Debug, Clone)]
pub(crate) struct RewindPlan {
    /// The highest row id that stays: every row of the thread above it belongs to the turn.
    pub boundary: i64,
    /// The highest row id the plan read, and how many rows it read above the boundary: the
    /// deletion must remove exactly those.
    pub last_row: i64,
    pub row_count: i64,
    /// The turn before it, which becomes the conversation's last; `None` when it was the first.
    pub previous_turn_id: Option<String>,
    /// The previous turn's rows, for a provider that cuts its session at that turn's end.
    pub previous_turn_rows: Vec<TranscriptRow>,
    /// The client id of the turn's first user message.
    pub prompt_client_id: Option<String>,
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
        .filter(|row| super::run_loop::is_user_message_type(row.item_type.as_deref()))
        .collect::<Vec<_>>();
    let prompt_client_id = user_rows.first().and_then(|row| client_id(&row.params));
    let mut said = 0;
    for row in user_rows {
        // Only the input tells a person's message from a system update, and a system update the
        // turn carried would be gone with it: the kernel delivers each one once.
        let Some(segments) = row
            .input_segments
            .as_deref()
            .and_then(super::turn_input::segments)
        else {
            return Err(
                "a message in this turn was recorded without its input, so it cannot be told \
                 apart from a system update"
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
        said += segments.len();
    }
    if said == 0 {
        return Err("this turn holds no message of yours to edit".into());
    }
    let suffix = rows.iter().filter(|row| row.id > boundary);
    let last_row = suffix.clone().map(|row| row.id).max().unwrap_or(boundary);
    let row_count = i64::try_from(suffix.count()).unwrap_or(i64::MAX);
    Ok(RewindPlan {
        boundary,
        last_row,
        row_count,
        previous_turn_id,
        previous_turn_rows,
        prompt_client_id,
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
