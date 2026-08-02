use std::borrow::Cow;

use sqlx::sqlite::SqlitePoolOptions;

fn migrator_through_0066() -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 66)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

#[tokio::test]
async fn upgrade_backfills_legacy_nonterminal_claim_context_to_empty_set() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open migration fixture");
    migrator_through_0066()
        .run(&pool)
        .await
        .expect("apply migrations through 0066");

    sqlx::query(
        r#"INSERT INTO tasks (
             id, wave_id, key, kind, goal, context_json, depends_on_json,
             status, created_at_ms, updated_at_ms
           ) VALUES (
             'wave-legacy:in-flight', 'wave-legacy', 'in-flight', 'codex',
             'legacy work', 'null', '[]', 'running', 1, 1
           )"#,
    )
    .execute(&pool)
    .await
    .expect("seed task created before context columns existed");

    crate::MIGRATOR
        .run(&pool)
        .await
        .expect("upgrade through context-freeze migration");
    let claim_context: Option<String> =
        sqlx::query_scalar("SELECT claim_context_json FROM tasks WHERE id = ?1")
            .bind("wave-legacy:in-flight")
            .fetch_one(&pool)
            .await
            .expect("read upgraded legacy task");
    assert_eq!(claim_context.as_deref(), Some("[]"));

    // The first correctness sweep treats a present empty closure as a
    // verified legacy claim, not as the fail-closed "snapshot missing"
    // case. Pin both halves of that distinction at the upgrade boundary.
    let frozen = claim_context
        .as_deref()
        .and_then(|json| serde_json::from_str::<Vec<calm_types::event::TaskContextRef>>(json).ok());
    assert_eq!(frozen, Some(Vec::new()));
    let first_sweep_material = frozen.is_none();
    assert!(
        !first_sweep_material,
        "the first sweep after upgrade must not mark a backfilled legacy task material"
    );
}
