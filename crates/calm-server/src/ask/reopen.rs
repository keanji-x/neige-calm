//! User-controlled reopening through an explicit, tagged ask action.
use super::StoredAsk;
use crate::error::{CalmError, Result};
use crate::event::{
    AskAction, AskAnswer, AskDelivery, AskQuestion, Event, EventScope, TrackUpdatedPayload,
};
use crate::ids::CardId;
use crate::model::{Track, TrackPatch};
use sqlx::{Sqlite, Transaction};

const OPTIONS: [&str; 2] = ["Reopen and continue", "Keep closed"];

/// Canonical choices belong to this action, not to a caller-supplied option label.
pub async fn ask_reopen_requested_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &CardId,
    questions: Vec<AskQuestion>,
) -> Result<(EventScope, Event)> {
    let [question] = questions.as_slice() else {
        return Err(CalmError::BadRequest(
            "a reopen ask needs one title, with options omitted".into(),
        ));
    };
    if !question.options.is_empty() {
        return Err(CalmError::BadRequest(
            "reopen choices are supplied by the kernel; omit options".into(),
        ));
    }
    let track_id = super::card_track_tx(tx, card).await?;
    let track = crate::db::sqlite::track_get_tx(tx, &track_id).await?;
    let closed_at = track
        .closed_at
        .ok_or_else(|| CalmError::Conflict("reopening requires a closed track".into()))?;
    crate::db::sqlite::track_require_reopenable_tx(tx, &track).await?;
    let pending: bool = sqlx::query_scalar(concat!(
        "SELECT EXISTS (SELECT 1 FROM events r WHERE r.scope_track = ?1 \
         AND r.kind = 'ask.requested' AND json_extract(r.payload, '$.action.kind') = 'reopen_track' AND ",
        super::ask_open_sql!(), ")"
    )).bind(track.id.as_str()).fetch_one(&mut **tx).await?;
    if pending {
        return Err(CalmError::Conflict(
            "a reopen question is already waiting for the user's answer".into(),
        ));
    }
    super::requested_tx(
        tx,
        card,
        vec![AskQuestion {
            title: question.title.clone(),
            options: OPTIONS.iter().map(|value| (*value).into()).collect(),
        }],
        AskDelivery::Wake,
        None,
        Some(AskAction::ReopenTrack { closed_at }),
    )
    .await
}

/// Only a tagged click on the canonical grant option authorizes this lifecycle effect.
/// Refusals are 400: 409 means a terminal ask and the existing drawer settles it.
pub(super) async fn answer_events_tx(
    tx: &mut Transaction<'_, Sqlite>,
    ask_id: i64,
    track: &Track,
    ask: &StoredAsk,
    answers: &[AskAnswer],
) -> Result<Vec<Event>> {
    let Some(AskAction::ReopenTrack { closed_at }) = ask.action else {
        return Ok(Vec::new());
    };
    if ask.delivery != AskDelivery::Wake
        || ask.questions.len() != 1
        || ask.questions[0].options != OPTIONS
    {
        return Err(CalmError::BadRequest(
            "invalid reopen question contract".into(),
        ));
    }
    if !matches!(answers, [AskAnswer::Option(0)]) {
        return Ok(Vec::new());
    }
    let superseded: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM events WHERE scope_track = ?1 AND id > ?2 \
         AND kind = 'track.updated' AND json_type(payload, '$.closed_at') = 'null')",
    )
    .bind(track.id.as_str())
    .bind(ask_id)
    .fetch_one(&mut **tx)
    .await?;
    if track.closed_at != Some(closed_at) || superseded {
        return Err(CalmError::BadRequest(
            "the closure changed; choose Keep closed and request reopening again".into(),
        ));
    }
    let restored = crate::db::sqlite::track_update_tx(
        tx,
        track.id.as_str(),
        TrackPatch {
            closed: Some(false),
            ..TrackPatch::default()
        },
    )
    .await
    .map_err(CalmError::from)
    .map_err(|error| match error {
        CalmError::Conflict(reason) => CalmError::BadRequest(reason),
        other => other,
    })?;
    Ok(vec![Event::TrackUpdated(TrackUpdatedPayload::new(
        restored, None,
    ))])
}
