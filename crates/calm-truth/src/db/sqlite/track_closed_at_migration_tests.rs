//! Migration 0123: a track is open or closed, and `closed_at` is its only state column.

use std::borrow::Cow;

use sqlx::sqlite::SqlitePoolOptions;

fn migrator_through(version: i64) -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= version)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

/// Every terminal lifecycle closes at its `terminal_at`; a live row stays open even with a stale stamp.
#[tokio::test]
async fn terminal_rows_close_at_terminal_at() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through(122)
        .run(&pool)
        .await
        .expect("apply migrations through 0122");

    sqlx::query(
        "INSERT INTO areas (id, name, color, sort, created_at, updated_at)
         VALUES ('area-1', 'a', '#000', 0, 1, 1)",
    )
    .execute(&pool)
    .await
    .expect("seed area");
    for (id, lifecycle, terminal_at) in [
        ("t-done", "done", Some(500_i64)),
        ("t-failed", "failed", Some(600)),
        ("t-canceled", "canceled", Some(700)),
        ("t-working", "working", Some(800)),
        ("t-draft", "draft", None),
    ] {
        sqlx::query(
            "INSERT INTO tracks (id, area_id, title, sort, lifecycle, terminal_at, created_at, updated_at)
             VALUES (?1, 'area-1', ?1, 0, ?2, ?3, 1, 1)",
        )
        .bind(id)
        .bind(lifecycle)
        .bind(terminal_at)
        .execute(&pool)
        .await
        .expect("seed track");
    }
    for kind in ["track.lifecycle_changed", "track.updated"] {
        sqlx::query(
            "INSERT INTO events (kind, payload, actor, at, scope_kind, scope_track)
             VALUES (?1, '{}', '\"User\"', 1, 'track', 't-done')",
        )
        .bind(kind)
        .execute(&pool)
        .await
        .expect("seed event");
    }

    crate::MIGRATOR
        .run(&pool)
        .await
        .expect("apply migration 0123");

    let rows: Vec<(String, Option<i64>)> =
        sqlx::query_as("SELECT id, closed_at FROM tracks ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("read tracks");
    assert_eq!(
        rows,
        vec![
            ("t-canceled".to_string(), Some(700)),
            ("t-done".to_string(), Some(500)),
            ("t-draft".to_string(), None),
            ("t-failed".to_string(), Some(600)),
            ("t-working".to_string(), None),
        ]
    );
    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM events ORDER BY id")
        .fetch_all(&pool)
        .await
        .expect("read events");
    assert_eq!(kinds, vec!["track.updated".to_string()]);
    let dropped: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('tracks') \
         WHERE name IN ('lifecycle', 'terminal_at', 'archived_at') \
         UNION ALL SELECT name FROM pragma_table_info('track_vcs_commits') WHERE name = 'lifecycle'",
    )
    .fetch_all(&pool)
    .await
    .expect("read columns");
    assert!(dropped.is_empty(), "{dropped:?}");
}
