//! Child assignment admission shared by declaration projection and transactional binding.
use sqlx::{Sqlite, Transaction};

use crate::error::{CalmError, Result};

/// Candidate returned children, before the active-binding fence. A continuation is not recovery:
/// every settled assignment must have explicitly handed back, and inflight child work is refused.
/// The caller supplies parent identity and a fresh task key separately.
pub fn child_track_handback_targets_sql() -> String {
    format!(
        "SELECT child.id,child.parent_track_id FROM tracks child \
    WHERE child.closed_at IS NULL \
    AND EXISTS(SELECT 1 FROM tasks prior WHERE prior.child_track_id=child.id \
        AND prior.track_id=child.parent_track_id AND prior.status='failed' \
        AND prior.status_detail='child-track-handed-back') \
    AND NOT EXISTS(SELECT 1 FROM tasks prior WHERE prior.child_track_id=child.id \
        AND prior.status NOT IN ('dispatched','running') \
        AND NOT (prior.status='failed' AND COALESCE(prior.status_detail,'')='child-track-handed-back')) \
    AND NOT EXISTS(SELECT 1 FROM current_tasks work WHERE work.track_id=child.id \
        AND work.status IN {})",
        super::RUNNING_WORK_STATUSES_SQL
    )
}

/// Shared by both creation and continuation. This SQL fence must remain independently testable:
/// declaration validation and candidate reads are not a substitute for transactional admission.
pub async fn task_bind_child_track_tx(
    tx: &mut Transaction<'_, Sqlite>,
    task_id: &str,
    parent_track_id: &str,
    child_track_id: &str,
    now: i64,
) -> Result<()> {
    let stamped = sqlx::query(
        "UPDATE tasks SET child_track_id=COALESCE(child_track_id,?1),updated_at_ms=?2 \
         WHERE id=?3 AND track_id=?4 AND status='dispatched' AND spawn='sub-track' \
           AND (child_track_id IS NULL OR child_track_id=?1) \
           AND EXISTS(SELECT 1 FROM tracks child WHERE child.id=?1 \
               AND child.parent_track_id=?4 AND child.closed_at IS NULL) \
           AND NOT EXISTS(SELECT 1 FROM tasks active WHERE active.child_track_id=?1 \
               AND active.id<>?3 AND active.status IN ('dispatched','running'))",
    )
    .bind(child_track_id)
    .bind(now)
    .bind(task_id)
    .bind(parent_track_id)
    .execute(&mut **tx)
    .await?;
    if stamped.rows_affected() != 1 {
        return Err(CalmError::Conflict("child-track-binding-refused"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn child_track_continue_migration_preserves_old_assignment_and_allows_new_round() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let old = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                crate::MIGRATOR
                    .iter()
                    .filter(|m| m.version < 195)
                    .cloned()
                    .collect(),
            ),
            ..sqlx::migrate::Migrator::DEFAULT
        };
        old.run(&pool).await.unwrap();
        sqlx::query("INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) VALUES('area','area','#000',0,'user',0,0)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('parent','area',\
            'parent',0,0,0),('child','area','child',1,0,0)")
            .execute(&pool).await.unwrap();
        sqlx::query("UPDATE tracks SET parent_track_id='parent' WHERE id='child'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,status_detail,spawn,\
            child_track_id,created_at_ms,updated_at_ms) VALUES('old','parent','old','codex','old goal',\
            '{}','failed','child-track-handed-back','sub-track','child',1,1)")
            .execute(&pool).await.unwrap();
        crate::MIGRATOR.run(&pool).await.unwrap();
        sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,spawn,child_track_id,\
            created_at_ms,updated_at_ms) VALUES('new','parent','new','codex','new goal','{}','dispatched',\
            'sub-track','child',2,2)")
            .execute(&pool).await.unwrap();
        let assignments: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT key,status,goal FROM tasks WHERE child_track_id='child' ORDER BY created_at_ms",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            assignments,
            vec![
                ("old".into(), "failed".into(), "old goal".into()),
                ("new".into(), "dispatched".into(), "new goal".into())
            ]
        );
    }
}
