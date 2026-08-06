use super::{SqlxRepo, cove_create_tx, cove_delete_tx, wave_create_tx, wave_update_tx};
use crate::db::RepoSyncDomainRaw;
use crate::model::{NewCove, NewWave, RequestTheme, WaveLifecycle, WavePatch};

async fn seed_cove(repo: &SqlxRepo, suffix: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let cove = cove_create_tx(
        &mut tx,
        NewCove {
            name: suffix.into(),
            color: "#000".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    cove.id.to_string()
}

async fn seed_wave(repo: &SqlxRepo, cove_id: &str, suffix: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            cove_id: cove_id.to_string().into(),
            title: suffix.into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            workflow_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        repo.wave_cove_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    wave.id.to_string()
}

#[tokio::test]
async fn acceptance_21_migration_uses_no_action_self_fk_and_partial_indexes() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let fk: (String,) = sqlx::query_as(
        "SELECT on_delete FROM pragma_foreign_key_list('waves') WHERE \"from\"='parent_wave_id'",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(fk.0, "NO ACTION");
    let index_sql: String =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name='idx_waves_parent_wave_id'")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert!(index_sql.contains("WHERE parent_wave_id IS NOT NULL"));
    let task_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('tasks')")
            .fetch_all(repo.pool())
            .await
            .unwrap();
    assert!(task_columns.contains(&"spawn".into()));
    assert!(task_columns.contains(&"child_wave_id".into()));
}

#[tokio::test]
async fn acceptance_20_repo_wave_delete_refuses_descendant_and_names_it() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo, "c").await;
    let parent = seed_wave(&repo, &cove, "p").await;
    let child = seed_wave(&repo, &cove, "ch").await;
    sqlx::query("UPDATE waves SET parent_wave_id=?1 WHERE id=?2")
        .bind(&parent)
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    let error = repo.wave_delete(&parent).await.unwrap_err();
    assert!(error.to_string().contains(&child));
}

#[tokio::test]
async fn acceptance_21b_cove_delete_removes_a_same_cove_wave_tree() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo, "c").await;
    let parent = seed_wave(&repo, &cove, "p").await;
    let child = seed_wave(&repo, &cove, "ch").await;
    sqlx::query("UPDATE waves SET parent_wave_id=?1 WHERE id=?2")
        .bind(&parent)
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    cove_delete_tx(&mut tx, &cove).await.unwrap();
    tx.commit().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM waves WHERE cove_id=?1")
        .bind(&cove)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn acceptance_17_raw_lifecycle_writer_refuses_reopen_of_referenced_child() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = seed_cove(&repo, "c").await;
    let parent = seed_wave(&repo, &cove, "p").await;
    let child = seed_wave(&repo, &cove, "ch").await;
    sqlx::query("UPDATE waves SET parent_wave_id=?1,lifecycle='done' WHERE id=?2")
        .bind(&parent)
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,status,declared_by,origin,spawn,child_wave_id,created_at_ms,updated_at_ms) \
         VALUES('parent:t',?1,'t','codex','g','{}','running','spec','block','sub-wave',?2,1,1)",
    )
    .bind(&parent).bind(&child).execute(repo.pool()).await.unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    let error = wave_update_tx(
        &mut tx,
        &child,
        WavePatch {
            lifecycle: Some(WaveLifecycle::Planning),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains(":t") && error.to_string().contains("cannot be reopened"));
}
