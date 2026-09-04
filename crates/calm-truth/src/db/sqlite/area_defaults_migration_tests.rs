//! Issue #1370 — forward-only Area default columns.

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
async fn migration_0087_preserves_existing_areas_with_no_defaults() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through(86)
        .run(&pool)
        .await
        .expect("apply migrations through 0086");
    sqlx::query(
        "INSERT INTO areas (id, name, color, sort, kind, created_at, updated_at)
         VALUES ('area-before-defaults', 'Existing', '#000', 1, 'user', 10, 20)",
    )
    .execute(&pool)
    .await
    .expect("seed pre-0087 Area");

    migrator_through(87)
        .run(&pool)
        .await
        .expect("apply migration 0087");

    let row: (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT name, default_template_id, default_cwd
         FROM areas WHERE id = 'area-before-defaults'",
    )
    .fetch_one(&pool)
    .await
    .expect("read upgraded Area");
    assert_eq!(row, ("Existing".into(), None, None));
}

#[tokio::test]
async fn migration_0087_rejects_a_relative_default_cwd() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through(87)
        .run(&pool)
        .await
        .expect("apply migrations through 0087");

    let error = sqlx::query(
        "INSERT INTO areas (
             id, name, color, sort, kind, default_cwd, created_at, updated_at
         ) VALUES ('bad-default', 'Bad', '#000', 1, 'user', 'relative/path', 10, 20)",
    )
    .execute(&pool)
    .await
    .expect_err("relative Area default must fail the storage constraint");
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "{error}"
    );
}
