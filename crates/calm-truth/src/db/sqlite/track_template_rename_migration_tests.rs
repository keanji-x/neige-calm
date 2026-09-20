//! Migration 0079 renames `tracks.workflow_id`/`workflow_input` -> `template_id`/`template_input`;
//! these pin that the rename preserves values (an `ADD COLUMN` + `DROP COLUMN` would blank old rows).

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

/// Non-NULL matters: with NULLs on both sides, `ADD COLUMN` + `DROP COLUMN`
/// is indistinguishable from `RENAME COLUMN`.
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

    // Pre-0079 schema: these column names are correct here and exempt from the rename sweep.
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

    // `run` applies only what is missing; a migrator holding *only* 0079 would be
    // rejected by sqlx's applied-version check.
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

/// Separate test so "renamed only one column" and "lost the values" are
/// distinguishable failures.
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
