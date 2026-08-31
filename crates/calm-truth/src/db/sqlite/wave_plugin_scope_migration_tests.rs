use std::borrow::Cow;

use sqlx::sqlite::SqlitePoolOptions;

fn migrator_through_0075() -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 75)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

fn migrator_through_0076() -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 76)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

/// #1110 S4 — 0076 backfill must not abort migrate on weird `workflows`
/// JSON, must copy the owning plugin id for a well-formed array of objects,
/// and must fail-closed (copy `workflow_id`) when no owner matches.
#[tokio::test]
async fn plugin_scope_backfill_skips_malformed_workflows_and_fail_closes_orphans() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through_0075()
        .run(&pool)
        .await
        .expect("apply migrations through 0075");

    sqlx::query(
        "INSERT INTO coves (id, name, color, sort, created_at, updated_at)
         VALUES ('cove-1', 'c', '#000', 0, 1, 1)",
    )
    .execute(&pool)
    .await
    .expect("seed cove");

    for (id, workflow_id) in [
        ("w-bound", Some("issue-development")),
        ("w-orphan", Some("no-such-workflow")),
        ("w-unbound", None),
    ] {
        // #1147 S1 — `cwd` dropped from the column list (migration-0018
        // `DEFAULT ''` covers it). This fixture runs against a schema stopped
        // at 0075, i.e. before the workspace columns exist, so it cannot go
        // through `wave_workspace_write_tx`; not naming `cwd` at all keeps it
        // consistent-by-construction instead of exempt. The plugin_scope
        // backfill under test never reads the workspace.
        sqlx::query(
            "INSERT INTO waves (id, cove_id, title, sort, lifecycle, workflow_id, created_at, updated_at)
             VALUES (?1, 'cove-1', 't', 0, 'draft', ?2, 1, 1)",
        )
        .bind(id)
        .bind(workflow_id)
        .execute(&pool)
        .await
        .unwrap_or_else(|error| panic!("seed wave {id}: {error}"));
    }

    let plugins = [
        ("invalid", "{not json}"),
        ("string-wf", r#"{"workflows":"issue-development"}"#),
        ("array-str", r#"{"workflows":["issue-development"]}"#),
        ("object-wf", r#"{"workflows":{"id":"issue-development"}}"#),
        (
            "dev.neige.git-forge",
            r#"{"workflows":[{"id":"issue-development"}]}"#,
        ),
    ];
    for (id, manifest) in plugins {
        sqlx::query(
            "INSERT INTO plugins (id, version, install_path, manifest, enabled, user_config, installed_at, updated_at)
             VALUES (?1, '0.1.0', '/tmp', ?2, 1, '{}', 1, 1)",
        )
        .bind(id)
        .bind(manifest)
        .execute(&pool)
        .await
        .unwrap_or_else(|error| panic!("seed plugin {id}: {error}"));
    }

    migrator_through_0076()
        .run(&pool)
        .await
        .expect("0076 must commit even with string/object/scalar workflows JSON");

    let bound: Option<String> =
        sqlx::query_scalar("SELECT plugin_scope FROM waves WHERE id = 'w-bound'")
            .fetch_one(&pool)
            .await
            .expect("bound plugin_scope");
    assert_eq!(
        bound.as_deref(),
        Some("dev.neige.git-forge"),
        "well-formed workflows[] array of objects must backfill the plugin id"
    );

    let orphan: Option<String> =
        sqlx::query_scalar("SELECT plugin_scope FROM waves WHERE id = 'w-orphan'")
            .fetch_one(&pool)
            .await
            .expect("orphan plugin_scope");
    assert_eq!(
        orphan.as_deref(),
        Some("no-such-workflow"),
        "unmatched bound row copies workflow_id so the gate is None, not All"
    );

    let unbound: Option<String> =
        sqlx::query_scalar("SELECT plugin_scope FROM waves WHERE id = 'w-unbound'")
            .fetch_one(&pool)
            .await
            .expect("unbound plugin_scope");
    assert_eq!(unbound, None, "unbound row stays NULL");
}
