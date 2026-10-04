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

#[tokio::test]
async fn migration_0140_renames_dev_references_and_preserves_saved_work() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    migrator_through(139).run(&pool).await.unwrap();
    for (id, template) in [
        ("old", Some("issue-development")),
        ("other", Some("small-change")),
        ("plain", None),
    ] {
        sqlx::query(
            "INSERT INTO areas
            (id, name, color, sort, created_at, updated_at, default_template_id, default_cwd)
            VALUES (?, ?, '#000', 0, 1, 2, ?, '/tmp/dev')",
        )
        .bind(id)
        .bind(id)
        .bind(template)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO tracks
            (id, area_id, title, sort, created_at, updated_at, template_id, template_input, plugin_scope)
            VALUES (?, ?, 'saved work', 0, 3, 4, ?, ?, 'dev.neige.git-forge')")
            .bind(id).bind(id).bind(template)
            .bind("{ \"issue_number\": 12, \"merge_policy\": \"hold-for-ratify\" }")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO cards
            (id, track_id, kind, role, sort, payload, created_at, updated_at)
            VALUES (?, ?, 'codex', 'planner', 0, ?, 5, 6)")
            .bind(id).bind(id)
            .bind("{ \"planner_harness\": true, \"template_context\": { \"version\": 1, \"title\": \"Issue development\", \"body\": \"original instructions\" }, \"approval\": \"saved\" }")
            .execute(&pool).await.unwrap();
    }
    sqlx::query(
        "INSERT INTO cards
        (id, track_id, kind, role, sort, payload, created_at, updated_at)
        VALUES ('report', 'old', 'track-report', 'reportcard', 1, ?, 7, 8)",
    )
    .bind("{ \"body\": \"saved report\", \"doc_rev\": 3 }")
    .execute(&pool)
    .await
    .unwrap();
    let before_tracks: Vec<(String, Option<String>, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, template_input, plugin_scope, updated_at FROM tracks ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let before_cards: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT id, payload, updated_at FROM cards ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    migrator_through(140).run(&pool).await.unwrap();
    for (id, expected) in [
        ("old", Some("dev")),
        ("other", Some("small-change")),
        ("plain", None),
    ] {
        let actual: Option<String> =
            sqlx::query_scalar("SELECT template_id FROM tracks WHERE id=?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(actual.as_deref(), expected, "track {id}");
        let actual: Option<String> =
            sqlx::query_scalar("SELECT default_template_id FROM areas WHERE id=?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(actual.as_deref(), expected, "area {id}");
    }
    let after_tracks = sqlx::query_as(
        "SELECT id, template_input, plugin_scope, updated_at FROM tracks ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(before_tracks, after_tracks);
    let after_cards = sqlx::query_as("SELECT id, payload, updated_at FROM cards ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(before_cards, after_cards);
    let cwd: String = sqlx::query_scalar("SELECT default_cwd FROM areas WHERE id='old'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cwd, "/tmp/dev");
}
