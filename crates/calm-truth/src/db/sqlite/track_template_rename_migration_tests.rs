//! #1209 PR-2 (design §3.3, test #17) — migration 0079 renames
//! `tracks.workflow_id` -> `tracks.template_id` and `tracks.workflow_input` ->
//! `tracks.template_input`.
//!
//! The rest of the slice only shows that the code works *after* the rename;
//! nothing in it would notice if the rename had thrown the old rows' values
//! away. A round-trip through `POST /api/tracks` writes a fresh row, so it
//! passes just as happily against an `ADD COLUMN` + `DROP COLUMN` migration
//! that silently blanks every track created before the upgrade. These fixtures
//! are the only carrier for "the rename preserves values".
//!
//! Recipe borrowed from `track_plugin_scope_migration_tests`: build a migrator
//! truncated to the version *before* the one under test, seed rows through the
//! old schema (so the old column names here are correct and must NOT be
//! renamed), then apply the migration under test and read the result back.

use std::borrow::Cow;

use sqlx::Row;
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

async fn columns_of_tracks(pool: &sqlx::SqlitePool) -> Vec<String> {
    sqlx::query("SELECT name FROM pragma_table_info('waves')")
        .fetch_all(pool)
        .await
        .expect("read tracks columns")
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect()
}

/// Seed a track at the 0078 schema with **non-NULL** values in both columns
/// about to be renamed, apply 0079, and read the values back through the new
/// names.
///
/// Non-NULL matters: with NULLs on both sides, an `ADD COLUMN` + `DROP COLUMN`
/// implementation is indistinguishable from `RENAME COLUMN`.
#[tokio::test]
async fn migration_0079_preserves_the_renamed_column_values() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through(78)
        .run(&pool)
        .await
        .expect("apply migrations through 0078");

    // Pre-0079 schema: these column names are the correct ones here and are
    // deliberately exempt from the rename sweep.
    let columns = columns_of_tracks(&pool).await;
    assert!(
        columns.iter().any(|c| c == "workflow_id") && columns.iter().any(|c| c == "workflow_input"),
        "fixture is not stopped before the rename; columns: {columns:?}"
    );

    sqlx::query(
        "INSERT INTO coves (id, name, color, sort, created_at, updated_at)
         VALUES ('area-1', 'c', '#000', 0, 1, 1)",
    )
    .execute(&pool)
    .await
    .expect("seed area");

    sqlx::query(
        "INSERT INTO waves (id, cove_id, title, sort, lifecycle, workflow_id, workflow_input, created_at, updated_at)
         VALUES ('w-1', 'area-1', 't', 0, 'draft', 'small-change', '{\"issue\":1209}', 1, 1)",
    )
    .execute(&pool)
    .await
    .expect("seed track with both legacy columns populated");

    // `run` applies only what is missing, so this applies exactly 0079 on top
    // of the 0078 state built above. A migrator holding *only* 0079 would be
    // rejected by sqlx's applied-version check (`VersionMissing`).
    migrator_through(79).run(&pool).await.expect("apply 0079");

    let row = sqlx::query("SELECT template_id, template_input FROM waves WHERE id='w-1'")
        .fetch_one(&pool)
        .await
        .expect("read the renamed columns back");
    assert_eq!(
        row.get::<Option<String>, _>("template_id").as_deref(),
        Some("small-change"),
        "RENAME COLUMN must carry the value across verbatim"
    );
    assert_eq!(
        row.get::<Option<String>, _>("template_input").as_deref(),
        Some("{\"issue\":1209}"),
        "RENAME COLUMN must carry the value across verbatim"
    );
}

/// The other half of the same migration: the old names must be **gone**.
///
/// Kept as its own test rather than two more assertions in the one above so
/// that "0079 renamed only one of the two columns" and "0079 lost the values"
/// are distinguishable failures.
#[tokio::test]
async fn migration_0079_removes_both_legacy_column_names() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through(79)
        .run(&pool)
        .await
        .expect("apply migrations through 0079");

    let columns = columns_of_tracks(&pool).await;
    for gone in ["workflow_id", "workflow_input"] {
        assert!(
            !columns.contains(&gone.to_string()),
            "{gone} survived migration 0079; columns: {columns:?}"
        );
    }
    for present in ["template_id", "template_input"] {
        assert!(
            columns.contains(&present.to_string()),
            "{present} missing after migration 0079; columns: {columns:?}"
        );
    }
}
