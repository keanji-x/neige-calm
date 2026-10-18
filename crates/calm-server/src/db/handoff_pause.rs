//! Read the original immutable session relationship, independently of live write authority.
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::harness::HarnessSnapshot;
use crate::harness::handoff_pause::{PauseQuery, PauseRequest};
use calm_types::worker::{SessionMode, WorkerContract};

#[derive(sqlx::FromRow)]
struct StoredPause {
    card_id: String,
    track_id: String,
    #[sqlx(try_from = "String")]
    mode: SessionMode,
    #[sqlx(try_from = "String")]
    contract: WorkerContract,
    handle_state_json: Option<String>,
}

fn conflict() -> CalmError {
    CalmError::Conflict("handoff pause claim or original storage binding changed".into())
}

/// Internal fact query only. It neither resolves a current owner nor grants authority.
pub async fn query_handoff_pause(repo: &dyn Repo, request: PauseRequest) -> Result<PauseQuery> {
    request.validate().map_err(|_| conflict())?;
    let pool = repo
        .sqlite_pool()
        .ok_or_else(|| CalmError::Internal("handoff query requires durable storage".into()))?;
    let (card, id, track) = request.identity();
    // One statement observes the complete storage relationship and snapshot atomically.
    // No deferred transaction can park holding read locks across subsequent awaits (#930).
    // Current pointers, active authority and registry state are deliberately absent.
    let session = sqlx::query_as::<_, StoredPause>(
        "SELECT ws.card_id, ws.track_id, ws.mode, ws.contract, ws.handle_state_json \
         FROM worker_sessions ws JOIN cards c ON c.id=ws.card_id AND c.track_id=ws.track_id \
         JOIN tracks t ON t.id=c.track_id WHERE ws.id=?1",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(conflict)?;
    if session.card_id != card.as_str()
        || session.track_id != track.as_str()
        || session.mode != SessionMode::Resumable
        || !matches!(
            session.contract,
            WorkerContract::Planner | WorkerContract::Assistant
        )
    {
        return Err(conflict());
    }
    let snapshot = HarnessSnapshot::parse_known(serde_json::from_str(
        &session.handle_state_json.ok_or_else(conflict)?,
    )?)
    .ok_or_else(conflict)?;
    let Some(receipt) = snapshot.handoff_pause.receipt() else {
        return Ok(PauseQuery::Unknown);
    };
    if !receipt.matches_binding(card, id, track) || receipt.request() != &request {
        return Err(conflict());
    }
    Ok(PauseQuery::Recorded(snapshot.handoff_pause))
}
