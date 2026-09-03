use crate::db::sqlite::SqlxRepo;
use crate::error::{Result, TruthError};
use crate::ids::TrackId;
use crate::worker::WorkerSessionId;

pub async fn delete_event_for_test(repo: &SqlxRepo, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM events WHERE id = ?1")
        .bind(id)
        .execute(repo.pool())
        .await?;
    Ok(())
}

pub async fn set_track_root_session_for_test(
    repo: &SqlxRepo,
    track: &TrackId,
    root: Option<&WorkerSessionId>,
) -> Result<()> {
    let result = sqlx::query("UPDATE tracks SET root_session_id = ?1 WHERE id = ?2")
        .bind(root.map(WorkerSessionId::as_str))
        .bind(track.as_str())
        .execute(repo.pool())
        .await?;
    if result.rows_affected() == 0 {
        return Err(TruthError::NotFound(format!("track {track}")));
    }
    Ok(())
}
