//! PR0-D (#679) — snapshot migration-replay harness acceptance test.
//!
//! Two suites:
//!
//!   1. `synthetic_fixture_replays_from_every_supported_version` — for every
//!      supported migration version N (0075+) in the chain: stage a temp DB
//!      at N, seed the synthetic core fixture
//!      (`tests/fixtures/migration_replay/core.json`, version-aware), replay
//!      to head via the production `Migrator::run` path, then (a) structurally
//!      diff the schema against a fresh head DB and (b) spot-check data survival.
//!
//!   2. `external_snapshot_replays_to_head` — point `NEIGE_SNAPSHOT_DB`
//!      at any (sanitized) production sqlite file and the same replay +
//!      schema-diff + integrity gates run against a private copy of it.
//!      Self-skips with an explicit marker when the env var is unset
//!      (codex-e2e self-skip pattern), so CI runs it as a no-op until a
//!      prod snapshot is provisioned.

use crate::support;

use support::migration_replay as harness;

#[test]
fn migration_replay_versions_start_at_supported_floor() {
    let versions = harness::migration_versions();
    assert_eq!(
        versions.first(),
        Some(&75),
        "migration 0075 is the oldest supported deployed schema"
    );
}

#[tokio::test]
async fn synthetic_fixture_replays_from_every_supported_version() {
    let fixture =
        harness::Fixture::from_json(include_str!("../fixtures/migration_replay/core.json"));
    let fresh = harness::fresh_head_fingerprint().await;
    // Guard against an empty-vs-empty trivially-green diff: the fresh
    // fingerprint must describe the schema we know is at head.
    for key in [
        "table:tracks",
        "table:cards",
        "table:worker_sessions",
        "table:operations",
        "table:events",
        "table:card_mcp_tokens",
        "table:tasks",
        "trigger:cards_role_validate_insert",
        "index:ws_one_active_per_card",
    ] {
        assert!(fresh.contains_key(key), "fresh fingerprint missing {key}");
    }
    assert!(
        !fresh.contains_key("table:runtimes"),
        "fresh head schema must not contain runtimes"
    );
    let versions = harness::migration_versions();
    assert!(
        versions.len() >= 11,
        "supported migration chain unexpectedly short: {versions:?}"
    );
    let head = *versions.last().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");

    for version in versions {
        let context = format!("staged at v{version:04}");
        let path = dir.path().join(format!("staged_{version}.sqlite"));
        let pool = harness::open_sqlite(&path).await;

        harness::stage_db_at(&pool, version).await;
        let report = harness::seed(&pool, &fixture, version).await;
        assert!(
            report.rows.len() >= 10,
            "[{context}] fixture seeded suspiciously few rows: {}",
            report.rows.len()
        );

        harness::replay_to_head(&pool).await;

        // Schema: staged+replayed must be structurally identical to fresh.
        let replayed = harness::schema_fingerprint(&pool).await;
        harness::assert_schema_matches(&replayed, &fresh, &context);

        // Data: every seeded row in a still-existing table survives.
        harness::assert_rows_survive(&pool, &report, &context).await;

        // Head stop sanity: at v=head the whole fixture (including rows
        // only expressible at head, e.g. the 'parked' operation) seeds
        // and replay is a checksum-validating no-op.
        if version == head {
            let parked: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE phase = 'parked'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(parked, 1, "[{context}] head-only parked operation seeded");
        }

        pool.close().await;
    }
}

/// Replay an externally provided (sanitized) production snapshot to head.
///
/// Usage: `NEIGE_SNAPSHOT_DB=/path/to/snapshot.sqlite cargo test -p
/// calm-server --test migration_suite migration_replay_harness::`. Works on a private copy;
/// the original file is never written.
#[tokio::test]
async fn external_snapshot_replays_to_head() {
    let Ok(snapshot) = std::env::var("NEIGE_SNAPSHOT_DB") else {
        eprintln!("[migration-replay] NEIGE_SNAPSHOT_DB not set; skipping external snapshot test");
        return;
    };
    let snapshot = std::path::PathBuf::from(snapshot);
    assert!(
        snapshot.is_file(),
        "NEIGE_SNAPSHOT_DB points at a missing file: {}",
        snapshot.display()
    );

    // Work on a copy — replay mutates. Carry sqlite sidecar files along
    // in case the snapshot was taken without a WAL checkpoint.
    let dir = tempfile::tempdir().expect("tempdir");
    let copy = dir.path().join("snapshot.sqlite");
    std::fs::copy(&snapshot, &copy).expect("copy snapshot");
    for suffix in ["-wal", "-shm"] {
        let sidecar = snapshot.with_file_name(format!(
            "{}{suffix}",
            snapshot.file_name().unwrap().to_string_lossy()
        ));
        if sidecar.is_file() {
            std::fs::copy(
                &sidecar,
                copy.with_file_name(format!("snapshot.sqlite{suffix}")),
            )
            .expect("copy snapshot sidecar");
        }
    }

    let pool = harness::open_sqlite(&copy).await;
    harness::replay_to_head(&pool).await;
    harness::assert_schema_matches_fresh(&pool, "external snapshot").await;

    let fk_violations: Vec<sqlx::sqlite::SqliteRow> = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(
        fk_violations.is_empty(),
        "external snapshot has {} foreign_key_check violation(s) after replay",
        fk_violations.len()
    );
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(integrity, "ok", "external snapshot integrity_check failed");
    pool.close().await;
}
