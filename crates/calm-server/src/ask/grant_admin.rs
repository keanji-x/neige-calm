//! A person's explicit click grants authority to the asker's own Track.
use super::StoredAsk;
use crate::error::{CalmError, Result};
use crate::event::{
    AskAction, AskAnswer, AskDelivery, AskQuestion, Event, EventScope, TrackUpdatedPayload,
};
use crate::ids::CardId;
use crate::model::Track;
use sqlx::{Sqlite, Transaction};

const OPTIONS: [&str; 2] = ["Grant admin", "Not now"];

pub async fn ask_grant_admin_requested_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &CardId,
    questions: Vec<AskQuestion>,
) -> Result<(EventScope, Event)> {
    let [question] = questions.as_slice() else {
        return Err(CalmError::BadRequest(
            "a grant_admin ask needs one title, with options omitted".into(),
        ));
    };
    if !question.options.is_empty() {
        return Err(CalmError::BadRequest(
            "grant_admin choices are supplied by the kernel; omit options".into(),
        ));
    }
    let track_id = super::card_track_tx(tx, card).await?;
    let track = crate::db::sqlite::track_get_tx(tx, &track_id).await?;
    require_grantable_tx(tx, &track).await?;
    let pending: bool = sqlx::query_scalar(concat!(
        "SELECT EXISTS (SELECT 1 FROM events r WHERE r.scope_track = ?1 \
         AND r.kind = 'ask.requested' AND json_extract(r.payload, '$.action.kind') = 'grant_admin' AND ",
        super::ask_open_sql!(), ")"
    )).bind(track.id.as_str()).fetch_one(&mut **tx).await?;
    if pending {
        return Err(CalmError::Conflict(
            "a grant_admin question is already waiting for the user's answer".into(),
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
        Some(AskAction::GrantAdmin),
    )
    .await
}

async fn require_grantable_tx(tx: &mut Transaction<'_, Sqlite>, track: &Track) -> Result<()> {
    if track.admin_granted_at.is_some() {
        return Err(CalmError::Conflict("already_admin".into()));
    }
    crate::area_admin::require_eligible_tx(&mut **tx, track).await
}

pub(super) async fn answer_events_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &Track,
    ask: &StoredAsk,
    answers: &[AskAnswer],
) -> Result<Vec<Event>> {
    if ask.action != Some(AskAction::GrantAdmin) {
        return Ok(Vec::new());
    }
    if ask.delivery != AskDelivery::Wake
        || ask.questions.len() != 1
        || ask.questions[0].options != OPTIONS
    {
        return Err(CalmError::BadRequest(
            "invalid grant_admin question contract".into(),
        ));
    }
    if !matches!(answers, [AskAnswer::Option(0)]) {
        return Ok(Vec::new());
    }
    require_grantable_tx(tx, track)
        .await
        .map_err(|error| CalmError::BadRequest(error.to_string()))?;
    let granted = crate::area_admin::set_tx(tx, track, true).await?;
    Ok(vec![Event::TrackUpdated(TrackUpdatedPayload::new(
        granted, None,
    ))])
}
