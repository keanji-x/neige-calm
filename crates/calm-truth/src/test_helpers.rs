use crate::db::sqlite::SqlxRepo;
use crate::error::{Result, TruthError};
use crate::ids::TrackId;
use crate::worker::WorkerSessionId;

pub async fn delete_event_for_test(repo: &SqlxRepo, id: i64) -> Result<()> {
    (repo.db())
        .writer()
        .execute(sqlx::query("DELETE FROM events WHERE id = ?1").bind(id))
        .await?;
    Ok(())
}

pub async fn set_track_root_session_for_test(
    repo: &SqlxRepo,
    track: &TrackId,
    root: Option<&WorkerSessionId>,
) -> Result<()> {
    let result = (repo.db())
        .writer()
        .execute(
            sqlx::query("UPDATE tracks SET root_session_id = ?1 WHERE id = ?2")
                .bind(root.map(WorkerSessionId::as_str))
                .bind(track.as_str()),
        )
        .await?;
    if result.rows_affected() == 0 {
        return Err(TruthError::NotFound(format!("track {track}")));
    }
    Ok(())
}
