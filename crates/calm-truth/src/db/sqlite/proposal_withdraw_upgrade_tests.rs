use std::borrow::Cow;

use sqlx::sqlite::SqlitePoolOptions;

use super::{SqlxRepo, check_no_unknown_future_migrations};
use crate::db::RepoEventWrite;
use crate::event::Event;

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

#[test]
fn historical_0065_migration_checksum_is_immutable() {
    // sqlx checksums the entire migration, including comments. Changing an
    // applied migration makes existing databases fail startup with VersionMismatch.
    const HISTORICAL_CHECKSUM: [u8; 48] = [
        0x6e, 0xd8, 0x67, 0x26, 0x24, 0xda, 0xfa, 0xc1, 0x03, 0xbd, 0x0d, 0x8b, 0xbc, 0x7c, 0x14,
        0x0b, 0xc0, 0x3e, 0x7e, 0xea, 0x57, 0x76, 0xbc, 0x5f, 0x4f, 0x2e, 0xfa, 0xd4, 0x93, 0x37,
        0x22, 0xab, 0x73, 0x49, 0x41, 0x1e, 0xde, 0xbb, 0xc5, 0x15, 0xdb, 0x20, 0x30, 0xc0, 0x35,
        0xed, 0xf4, 0x22,
    ];
    let migration = crate::MIGRATOR
        .iter()
        .find(|migration| migration.version == 65)
        .expect("migration 0065 must exist");

    assert_eq!(migration.checksum.as_ref(), HISTORICAL_CHECKSUM);
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
           ('proposal.submitted', ?1, 'plugin:dev.example', 1, 12, 'track', 'track-legacy'),
           ('proposal.resolved', ?2, 'user', 2, 12, 'track', 'track-legacy')"#,
    )
    .bind(
        serde_json::json!({
            "track_id": "track-legacy",
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
            "track_id": "track-legacy",
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
               'proposal-legacy', 'track-legacy', 'dev.example', 'report', 'ah1:legacy',
               '[]', 'historical proposal', 'legacy-idem', 'rejected', 1, 2, 1, 2
           )"#,
    )
    .execute(&pool)
    .await
    .expect("seed proposals projection row");

    pool.close().await;

    let repo = SqlxRepo::open(&url)
        .await
        .expect("new binary starts on and upgrades a database at 0065");
    assert_historical_proposal_replay(&repo).await;
    let proposals_table: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'proposals'",
    )
    .fetch_optional(&repo.pool)
    .await
    .expect("inspect upgraded schema");
    assert!(
        proposals_table.is_none(),
        "migration 0066 must drop the proposals projection"
    );

    repo.pool.close().await;

    let repo = SqlxRepo::open(&url)
        .await
        .expect("new binary still starts after migration 0066");
    assert_historical_proposal_replay(&repo).await;

    let rollback_error = check_no_unknown_future_migrations(&repo.pool, &old_migrator)
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
