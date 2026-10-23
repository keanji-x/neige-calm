//! Strict persisted event facts for consumers that commit a durable prefix.
//! Audit readers deliberately skip damaged rows; these readers cannot do so.
use crate::db::TrackEvent;
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqliteConnection, SqlitePool, Transaction};

#[derive(Clone, Debug)]
pub struct EventInputFact {
    pub envelope: TrackEvent,
    pub fingerprint: String,
}
#[derive(FromRow)]
struct Stored {
    id: i64,
    kind: String,
    payload: String,
    actor: String,
    at: i64,
    event_version: i64,
    scope_kind: Option<String>,
    scope_area: Option<String>,
    scope_track: Option<String>,
    scope_card: Option<String>,
}
const COLUMNS: &str =
    "id,kind,payload,actor,at,event_version,scope_kind,scope_area,scope_track,scope_card";
fn decode(row: Stored) -> Result<EventInputFact> {
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            row.id,
            &row.kind,
            &row.payload,
            &row.actor,
            row.at,
            row.event_version,
            &row.scope_kind,
            &row.scope_area,
            &row.scope_track,
            &row.scope_card,
        ))?)
    );
    let actor: ActorId = serde_json::from_str(&row.actor)?;
    let event = Event::from_stored_kind_and_payload(
        &row.kind,
        serde_json::from_str(&row.payload)?,
        u32::try_from(row.event_version)
            .map_err(|_| CalmError::Internal("invalid stored event version".into()))?,
    )
    .map_err(|error| CalmError::Internal(format!("event {}: {error}", row.id)))?;
    let scope = EventScope::from_row(
        row.scope_kind.as_deref(),
        row.scope_area.as_deref(),
        row.scope_track.as_deref(),
        row.scope_card.as_deref(),
    );
    if row.id <= 0
        || row.event_version <= 0
        || Some(scope.kind()) != row.scope_kind.as_deref()
        || scope.track_id().is_none()
        || scope.area_id().is_none()
    {
        return Err(CalmError::Conflict(
            "input event has no exact stored Track scope",
        ));
    }
    Ok(EventInputFact {
        envelope: TrackEvent {
            id: row.id,
            at: row.at,
            actor,
            scope,
            event,
        },
        fingerprint,
    })
}

pub async fn track_prefix(
    pool: &SqlitePool,
    track: &str,
    kinds: &[&str],
    after: i64,
    through: Option<i64>,
) -> Result<Vec<EventInputFact>> {
    let mut connection = pool.acquire().await?;
    track_prefix_on(&mut connection, track, kinds, after, through).await
}
pub async fn track_prefix_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &str,
    kinds: &[&str],
    after: i64,
    through: Option<i64>,
) -> Result<Vec<EventInputFact>> {
    track_prefix_on(tx, track, kinds, after, through).await
}
async fn track_prefix_on(
    connection: &mut SqliteConnection,
    track: &str,
    kinds: &[&str],
    after: i64,
    through: Option<i64>,
) -> Result<Vec<EventInputFact>> {
    let mut query =
        QueryBuilder::<Sqlite>::new(format!("SELECT {COLUMNS} FROM events WHERE scope_track="));
    query.push_bind(track).push(" AND id>").push_bind(after);
    if let Some(through) = through {
        query.push(" AND id<=").push_bind(through);
    }
    query.push(" AND kind IN (");
    let mut separated = query.separated(",");
    for kind in kinds {
        separated.push_bind(*kind);
    }
    separated.push_unseparated(") ORDER BY id");
    query
        .build_query_as::<Stored>()
        .fetch_all(connection)
        .await?
        .into_iter()
        .map(decode)
        .collect()
}

pub async fn event_input_tx(tx: &mut Transaction<'_, Sqlite>, id: i64) -> Result<EventInputFact> {
    read(tx, id).await
}
async fn read(connection: &mut SqliteConnection, id: i64) -> Result<EventInputFact> {
    let row = sqlx::query_as::<_, Stored>(&format!("SELECT {COLUMNS} FROM events WHERE id=?1"))
        .bind(id)
        .fetch_optional(connection)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("input event {id}")))?;
    decode(row)
}
