//! Shared ratify workflow state derived from durable events.

use crate::error::CalmError;
use crate::ids::TrackId;
use sqlx::{Sqlite, Transaction};

pub(crate) async fn ratify_request_pending_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &TrackId,
) -> Result<bool, CalmError> {
    let kind = sqlx::query_scalar::<_, String>(
        "SELECT kind FROM events \
         WHERE scope_track = ?1 AND kind IN ('ratify.requested', 'ratify.resolved') \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(track_id.as_str())
    .fetch_optional(&mut **tx)
    .await?;

    Ok(kind.as_deref() == Some("ratify.requested"))
}
