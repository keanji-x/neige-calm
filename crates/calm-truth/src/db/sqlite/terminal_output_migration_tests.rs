//! #1456 — forward-only durable terminal output evidence migration.

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

#[tokio::test]
async fn migration_0091_marks_pre_capture_exits_as_truncated() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through(90)
        .run(&pool)
        .await
        .expect("apply migrations through released 0090");
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&pool)
        .await
        .unwrap();
    for (id, exit_code) in [("exited", Some(0_i64)), ("running", None)] {
        sqlx::query(
            "INSERT INTO terminals \
             (id,card_id,program,cwd,env,pid,theme_fg,theme_bg,exit_code,signal_killed,created_at) \
             VALUES (?1,?2,'sh','/tmp','{}',NULL,'1,1,1','2,2,2',?3,0,1)",
        )
        .bind(id)
        .bind(format!("card-{id}"))
        .bind(exit_code)
        .execute(&pool)
        .await
        .unwrap();
    }

    migrator_through(91)
        .run(&pool)
        .await
        .expect("apply migration 0091");

    let rows: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT id,pty_output,pty_output_truncated FROM terminals ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            ("exited".into(), "".into(), 1),
            ("running".into(), "".into(), 0)
        ]
    );
    let error = sqlx::query("UPDATE terminals SET pty_output_truncated=2 WHERE id='running'")
        .execute(&pool)
        .await
        .expect_err("truncation flag is boolean");
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "{error}"
    );
}
