use crate::db::sqlite::SqlxRepo;
use crate::error::{Result, TruthError};
use crate::ids::WaveId;
use crate::worker::WorkerSessionId;

pub async fn delete_event_for_test(repo: &SqlxRepo, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM events WHERE id = ?1")
        .bind(id)
        .execute(repo.pool())
        .await?;
    Ok(())
}

pub async fn set_wave_root_session_for_test(
    repo: &SqlxRepo,
    wave: &WaveId,
    root: Option<&WorkerSessionId>,
) -> Result<()> {
    let result = sqlx::query("UPDATE waves SET root_session_id = ?1 WHERE id = ?2")
        .bind(root.map(WorkerSessionId::as_str))
        .bind(wave.as_str())
        .execute(repo.pool())
        .await?;
    if result.rows_affected() == 0 {
        return Err(TruthError::NotFound(format!("wave {wave}")));
    }
    Ok(())
}
