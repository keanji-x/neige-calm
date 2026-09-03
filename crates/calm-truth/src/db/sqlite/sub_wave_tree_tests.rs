use super::{SqlxRepo, area_create_tx, area_delete_tx, wave_create_tx, wave_update_tx};
use crate::db::RepoSyncDomainRaw;
use crate::model::{NewArea, NewWave, RequestTheme, WaveLifecycle, WavePatch};

async fn seed_area(repo: &SqlxRepo, suffix: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let area = area_create_tx(
        &mut tx,
        NewArea {
            name: suffix.into(),
            color: "#000".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    area.id.to_string()
}

async fn seed_wave(repo: &SqlxRepo, area_id: &str, suffix: &str) -> String {
    let mut tx = repo.pool().begin().await.unwrap();
    let wave = wave_create_tx(
        &mut tx,
        NewWave {
            area_id: area_id.to_string().into(),
            title: suffix.into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &crate::db::sqlite::WaveWorkspacePlan::AttachedFromCwd,
        repo.wave_area_cache(),
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
    let area = seed_area(&repo, "c").await;
    let parent = seed_wave(&repo, &area, "p").await;
    let child = seed_wave(&repo, &area, "ch").await;
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
async fn acceptance_21b_area_delete_removes_a_same_area_wave_tree() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo, "c").await;
    let parent = seed_wave(&repo, &area, "p").await;
    let child = seed_wave(&repo, &area, "ch").await;
    sqlx::query("UPDATE waves SET parent_wave_id=?1 WHERE id=?2")
        .bind(&parent)
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    area_delete_tx(&mut tx, &area).await.unwrap();
    tx.commit().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM waves WHERE area_id=?1")
        .bind(&area)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn acceptance_21c_cross_area_edge_is_a_loud_delete_tripwire() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area_a = seed_area(&repo, "a").await;
    let area_b = seed_area(&repo, "b").await;
    let parent = seed_wave(&repo, &area_a, "p").await;
    let child = seed_wave(&repo, &area_b, "ch").await;
    // Deliberately bypass the production adapter: this is poison data that
    // pins the self-FK's NO ACTION behavior, complementary to the real-adapter
    // invariant that no such edge is normally written.
    sqlx::query("UPDATE waves SET parent_wave_id=?1 WHERE id=?2")
        .bind(&parent)
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    let mismatch: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM waves child JOIN waves parent ON parent.id=child.parent_wave_id \
         WHERE child.area_id<>parent.area_id",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(mismatch, 1);
    let mut tx = repo.pool().begin().await.unwrap();
    let error = area_delete_tx(&mut tx, &area_a).await.unwrap_err();
    assert!(
        error.to_string().contains("FOREIGN KEY"),
        "cross-area edge must fail loudly: {error}"
    );
    tx.rollback().await.unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM waves WHERE id IN (?1,?2)")
        .bind(&parent)
        .bind(&child)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(remaining, 2, "failed area delete must preserve both waves");
}

#[tokio::test]
async fn acceptance_17_raw_lifecycle_writer_refuses_reopen_of_referenced_child() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = seed_area(&repo, "c").await;
    let parent = seed_wave(&repo, &area, "p").await;
    let child = seed_wave(&repo, &area, "ch").await;
    sqlx::query("UPDATE waves SET parent_wave_id=?1,lifecycle='done' WHERE id=?2")
        .bind(&parent)
        .bind(&child)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tasks(id,wave_id,key,kind,goal,context_json,status,declared_by,spawn,child_wave_id,created_at_ms,updated_at_ms) \
         VALUES('parent:t',?1,'t','codex','g','{}','running','spec','sub-wave',?2,1,1)",
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
