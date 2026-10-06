//! Migration 0153 rewrites the dev merge policy `hold-for-ratify` to `ask` in `tracks.template_input`
//! and touches nothing else: other keys, other values, other rows and `updated_at` stay as stored.

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

/// Stored inputs as 4140 holds them (minified JSON text), with the expected value after 0153.
const ROWS: &[(&str, Option<&str>, Option<&str>)] = &[
    (
        "asks",
        Some(
            r#"{"issue_number":1870,"merge_policy":"hold-for-ratify","notes":"was hold-for-ratify","repo":"o/r"}"#,
        ),
        Some(
            r#"{"issue_number":1870,"merge_policy":"ask","notes":"was hold-for-ratify","repo":"o/r"}"#,
        ),
    ),
    (
        "auto",
        Some(r#"{"merge_policy":"auto-merge","notes":"was hold-for-ratify"}"#),
        Some(r#"{"merge_policy":"auto-merge","notes":"was hold-for-ratify"}"#),
    ),
    (
        "absent",
        Some(r#"{"issue_number":7}"#),
        Some(r#"{"issue_number":7}"#),
    ),
    (
        "other-key",
        Some(r#"{"policy":"hold-for-ratify"}"#),
        Some(r#"{"policy":"hold-for-ratify"}"#),
    ),
    (
        "not-text",
        Some(r#"{"merge_policy":["hold-for-ratify"]}"#),
        Some(r#"{"merge_policy":["hold-for-ratify"]}"#),
    ),
    ("json-null", Some("null"), Some("null")),
    (
        "invalid",
        Some(r#"{"merge_policy":"hold-for-ratify""#),
        Some(r#"{"merge_policy":"hold-for-ratify""#),
    ),
    ("none", None, None),
];

async fn inputs(pool: &sqlx::SqlitePool) -> Vec<(String, Option<String>, i64)> {
    sqlx::query_as("SELECT id, template_input, updated_at FROM tracks ORDER BY id")
        .fetch_all(pool)
        .await
        .expect("read tracks back")
}

#[tokio::test]
async fn migration_0153_renames_only_the_hold_for_ratify_merge_policy() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through(152)
        .run(&pool)
        .await
        .expect("apply migrations through 0152");
    sqlx::query(
        "INSERT INTO areas (id, name, color, sort, created_at, updated_at)
         VALUES ('area', 'a', '#000', 0, 1, 1)",
    )
    .execute(&pool)
    .await
    .expect("seed area");
    for (id, stored, _) in ROWS {
        sqlx::query(
            "INSERT INTO tracks (id, area_id, title, sort, created_at, updated_at, template_id, template_input)
             VALUES (?, 'area', 't', 0, 3, 4, 'dev', ?)",
        )
        .bind(id)
        .bind(stored)
        .execute(&pool)
        .await
        .expect("seed track");
    }

    migrator_through(153).run(&pool).await.expect("apply 0153");

    let mut expected: Vec<(String, Option<String>, i64)> = ROWS
        .iter()
        .map(|(id, _, after)| (id.to_string(), after.map(str::to_string), 4))
        .collect();
    expected.sort();
    assert_eq!(inputs(&pool).await, expected);

    // Running the statement again changes nothing.
    let migration = crate::MIGRATOR
        .iter()
        .find(|m| m.version == 153)
        .expect("0153 is embedded");
    sqlx::raw_sql(&migration.sql)
        .execute(&pool)
        .await
        .expect("rerun 0153");
    assert_eq!(inputs(&pool).await, expected);
}
