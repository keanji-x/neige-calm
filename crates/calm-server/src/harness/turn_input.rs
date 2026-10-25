//! The one owner of "this turn's input" (#2130 D17), selected by persistent storage format.
//! Historical segment decoding is also shared with the rewind planner.

use sqlx::{Sqlite, Transaction};

use crate::db::sqlite::{conversation, transcript_latest_turn_input_tx};
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

/// The exact current canonical execution, or newest Historical issuance/steer.
/// A canonical miss or error never falls back to a Historical projection.
pub(crate) async fn latest(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    worker_session_id: &str,
) -> Result<Option<TurnInput>> {
    if conversation::card_has_canonical_storage(tx, card_id).await? {
        return Ok(
            conversation::current_turn_input_tx(tx, card_id, worker_session_id)
                .await?
                .map(|(created_at_ms, has_user_segment)| TurnInput {
                    created_at_ms,
                    has_user_segment,
                }),
        );
    }
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
