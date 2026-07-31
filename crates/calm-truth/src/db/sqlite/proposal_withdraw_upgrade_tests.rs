use std::borrow::Cow;

use sqlx::sqlite::SqlitePoolOptions;

use super::{SqlxRepo, check_no_unknown_future_migrations};
use crate::card_role_cache::CardRoleCache;
use crate::db::RepoEventWrite;
use crate::event::Event;
use crate::wave_cove_cache::WaveCoveCache;

fn migrator_through_0065() -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 65)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    }
}

fn repo_for_pool(pool: sqlx::SqlitePool) -> SqlxRepo {
    SqlxRepo {
        pool,
        card_role_cache: CardRoleCache::new(),
        wave_cove_cache: WaveCoveCache::new(),
        _memory_cache_anchor: None,
    }
}

async fn assert_historical_proposal_replay(repo: &SqlxRepo) {
    let events = repo
        .events_since(0, 100)
        .await
        .expect("historical events_since succeeds");
    assert_eq!(events.len(), 2, "proposal history must not lose rows");
    assert!(matches!(events[0].3, Event::ProposalSubmitted { .. }));
    assert!(matches!(events[1].3, Event::ProposalResolved { .. }));

    // Exercise the same typed reconstruction used by the replay loader.
    for (_, _, _, event) in events {
        Event::from_kind_and_payload(event.kind_tag(), event.payload_value())
            .expect("historical proposal event replays without panic");
    }
}

#[tokio::test]
async fn proposal_projection_upgrade_and_unsupported_rollback_are_explicit() {
    let dir = tempfile::tempdir().expect("temporary database directory");
    let db_path = dir.path().join("proposal-upgrade.sqlite");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("open fixture database");
    let old_migrator = migrator_through_0065();
    old_migrator
        .run(&pool)
        .await
        .expect("apply fixture through migration 0065");

    let highest: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("read fixture migration version");
    assert_eq!(highest, 65, "fixture must stop at migration 0065");

    sqlx::query(
        r#"INSERT INTO events (kind, payload, actor, at, event_version, scope_kind, scope_wave)
           VALUES
           ('proposal.submitted', ?1, 'plugin:dev.example', 1, 12, 'wave', 'wave-legacy'),
           ('proposal.resolved', ?2, 'user', 2, 12, 'wave', 'wave-legacy')"#,
    )
    .bind(
        serde_json::json!({
            "wave_id": "wave-legacy",
            "proposal_id": "proposal-legacy",
            "plugin_id": "dev.example",
            "subject_kind": "report",
            "base_doc_heads": "ah1:legacy",
            "ops": [{
                "op": "delete_block",
                "block_id": "block-legacy",
                "if_rev": 1
            }],
            "note": "historical proposal",
            "idem_key": "legacy-idem"
        })
        .to_string(),
    )
    .bind(
        serde_json::json!({
            "wave_id": "wave-legacy",
            "proposal_id": "proposal-legacy",
            "plugin_id": "dev.example",
            "decision": "rejected"
        })
        .to_string(),
    )
    .execute(&pool)
    .await
    .expect("seed historical proposal events");

    sqlx::query(
        r#"INSERT INTO proposals (
               proposal_id, wave_id, plugin_id, subject_kind, base_doc_heads,
               ops, note, idem_key, status, submitted_event_id,
               resolved_event_id, created_at, resolved_at
           ) VALUES (
               'proposal-legacy', 'wave-legacy', 'dev.example', 'report', 'ah1:legacy',
               '[]', 'historical proposal', 'legacy-idem', 'rejected', 1, 2, 1, 2
           )"#,
    )
    .execute(&pool)
    .await
    .expect("seed proposals projection row");

    // This is the exact guard called by SqlxRepo::open before migrations.
    check_no_unknown_future_migrations(&pool, &crate::MIGRATOR)
        .await
        .expect("new binary accepts a database at 0065");
    let repo = repo_for_pool(pool.clone());
    assert_historical_proposal_replay(&repo).await;

    crate::MIGRATOR
        .run(&pool)
        .await
        .expect("upgrade fixture through migration 0066");
    let proposals_table: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'proposals'",
    )
    .fetch_optional(&pool)
    .await
    .expect("inspect upgraded schema");
    assert!(
        proposals_table.is_none(),
        "migration 0066 must drop the proposals projection"
    );

    check_no_unknown_future_migrations(&pool, &crate::MIGRATOR)
        .await
        .expect("new binary still accepts the database after 0066");
    assert_historical_proposal_replay(&repo).await;

    let rollback_error = check_no_unknown_future_migrations(&pool, &old_migrator)
        .await
        .expect_err("old binary must refuse a database upgraded through 0066")
        .to_string();
    assert!(
        rollback_error.contains("migration 66")
            && rollback_error.contains("refusing to boot")
            && rollback_error.contains("downgrade is not supported"),
        "rollback rejection must be recognizable: {rollback_error}"
    );
    assert!(
        !rollback_error.contains("no such table"),
        "rollback must fail at the migration guard: {rollback_error}"
    );
}
