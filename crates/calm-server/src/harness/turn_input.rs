//! The one owner of "this turn's input" (#2130 D17): the newest turn-input row a Planner session
//! wrote, and the decode of its stored segments that the rewind planner shares.

use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::transcript_latest_turn_input_tx;
use crate::error::{CalmError, Result};
use crate::model::{HarnessInputPresentation, HarnessInputSegment};

/// The input of the newest turn a session issued: when it was recorded and whether a person spoke in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TurnInput {
    pub created_at_ms: i64,
    pub has_user_segment: bool,
}

/// The stored `input_segments` JSON of a transcript row, or `None` when it does not decode.
pub(crate) fn segments(json: &str) -> Option<Vec<HarnessInputSegment>> {
    serde_json::from_str(json).ok()
}

/// The caller session's newest turn input, read in the caller's transaction; `None` when the session
/// has recorded none (only an older binary's turn in flight at upgrade).
pub(crate) async fn latest(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    worker_session_id: &str,
) -> Result<Option<TurnInput>> {
    let Some((created_at_ms, json)) =
        transcript_latest_turn_input_tx(tx, card_id, worker_session_id).await?
    else {
        return Ok(None);
    };
    let segments = segments(&json).ok_or_else(|| {
        CalmError::Internal(format!(
            "turn input of session {worker_session_id} has undecodable segments"
        ))
    })?;
    Ok(Some(TurnInput {
        created_at_ms,
        has_user_segment: segments
            .iter()
            .any(|segment| segment.presentation == HarnessInputPresentation::User),
    }))
}
