//! 0117 (#1791): every Planner card, whatever its payload shape, gains
//! `planner_provider: "codex"`; no other card and no other payload key changes.

use calm_truth::MIGRATOR;
use sqlx::{Connection, SqliteConnection, migrate::Migrate, sqlite::SqliteConnectOptions};

async fn apply_through(db: &mut SqliteConnection, last: i64) {
    let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
        .fetch_all(&mut *db)
        .await
        .unwrap();
    for migration in MIGRATOR.iter().filter(|m| {
        m.version <= last && !applied.contains(&m.version) && !m.migration_type.is_down_migration()
    }) {
        db.apply(migration).await.unwrap();
    }
}

#[tokio::test]
async fn planner_cards_of_every_shape_are_stamped_codex_and_nothing_else_moves() {
    let mut db = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    db.ensure_migrations_table().await.unwrap();
    apply_through(&mut db, 116).await;
    sqlx::raw_sql(
        "INSERT INTO areas (id, name, color, sort, created_at, updated_at) VALUES ('a', 'a', '#000', 0, 1, 1);
         INSERT INTO tracks (id, area_id, title, sort, created_at, updated_at) VALUES
           ('t1', 'a', 't1', 0, 1, 1), ('t2', 'a', 't2', 1, 1, 1);
         INSERT INTO cards (id, track_id, kind, sort, payload, created_at, updated_at, role) VALUES
           ('marker', 't1', 'codex', 0, '{\"schemaVersion\":1,\"codex_source\":\"shared\",\"planner_harness\":true}', 1, 1, 'planner'),
           ('legacy', 't2', 'codex', 0, '{\"schemaVersion\":1,\"harness\":{\"snapshotVersion\":0,\"pendingQueue\":[]}}', 1, 1, 'planner'),
           ('worker', 't1', 'codex', 1, '{\"schemaVersion\":1}', 1, 1, 'worker'),
           ('assistant', 't1', 'codex', 2, '{\"schemaVersion\":1,\"harness_profile\":\"assistant\"}', 1, 1, 'assistant'),
           ('report', 't1', 'track-report', 3, '{\"schemaVersion\":4}', 1, 1, 'reportcard');",
    )
    .execute(&mut db)
    .await
    .unwrap();
    let before: Vec<(String, String)> = sqlx::query_as("SELECT id, payload FROM cards ORDER BY id")
        .fetch_all(&mut db)
        .await
        .unwrap();

    apply_through(&mut db, 117).await;

    let after: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, role, payload FROM cards ORDER BY id")
            .fetch_all(&mut db)
            .await
            .unwrap();
    for ((id, old), (_, role, new)) in before.iter().zip(&after) {
        let old: serde_json::Value = serde_json::from_str(old).unwrap();
        let new: serde_json::Value = serde_json::from_str(new).unwrap();
        if role == "planner" {
            let mut expected = old.clone();
            expected["planner_provider"] = serde_json::json!("codex");
            assert_eq!(new, expected, "planner card {id}");
        } else {
            assert_eq!(new, old, "non-planner card {id} must not change");
        }
    }
    let stamped: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cards WHERE json_extract(payload, '$.planner_provider') = 'codex'",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(stamped, 2, "exactly the two planner cards");
}
