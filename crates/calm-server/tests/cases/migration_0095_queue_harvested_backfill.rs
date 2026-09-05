//! #1449 — migration 0095 backfills `queue_harvested_at_ms` over RETIRED rows
//! only.
//!
//! The backfill exists so that the first restart after the upgrade does not
//! replay a sentence from days ago into a brand new thread: a legacy
//! `superseded` row can carry a non-empty `pending_queue`, and that stranding is
//! #1449 itself.
//!
//! The `state` predicate is what keeps it from doing the opposite of its job.
//! Boot recovery reuses a runtime's id, so a row that is `idle` at upgrade time
//! goes on living afterwards and receives NEW input — `observe_user_message_durable`
//! writes that very row. Stamping it during the upgrade would make the first
//! sentence a user types AFTER the upgrade permanently unharvestable: exactly
//! the silent loss this slice exists to end, introduced by the fix for it.
//!
//! Staged one migration short of head and replayed the way production boot
//! replays, so the assertion is about the shipped statement rather than about a
//! copy of it.

use crate::support::migration_replay::{replay_to_head, stage_db_at};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::str::FromStr;

const STAGED_VERSION: i64 = 94;

async fn staged_pool(dir: &std::path::Path) -> sqlx::SqlitePool {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", dir.join("t.db").display()))
        .unwrap()
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(false);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap()
}

/// One card per row: `worker_sessions` carries a partial unique index over a
/// card's ACTIVE row, so two live rows cannot share a card even in a fixture.
async fn insert_runtime(pool: &sqlx::SqlitePool, id: &str, state: &str) {
    sqlx::query(
        r#"INSERT INTO worker_sessions
             (id, track_id, card_id, provider, mode, contract, state,
              handle_state_json, created_at_ms, updated_at_ms)
           VALUES (?1, 'track-1', ?1, 'codex', 'resumable', 'planner', ?2,
                   '{"mode":"harness","pending_queue":[{"type":"user_message","text":"say it"}]}',
                   1000, 1000)"#,
    )
    .bind(id)
    .bind(state)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("seed {id} as {state}: {e}"));
}

async fn stamp(pool: &sqlx::SqlitePool, id: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT queue_harvested_at_ms FROM worker_sessions WHERE id = ?1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_backfill_retires_legacy_queues_without_touching_a_live_one() {
    let dir = tempfile::TempDir::new().unwrap();
    let pool = staged_pool(dir.path()).await;
    stage_db_at(&pool, STAGED_VERSION).await;

    // Every state the column has to be decided for, seeded before the upgrade.
    insert_runtime(&pool, "retired", "superseded").await;
    insert_runtime(&pool, "live-idle", "idle").await;
    insert_runtime(&pool, "live-turn", "turn_pending").await;
    insert_runtime(&pool, "dead-failed", "failed").await;
    insert_runtime(&pool, "dead-exited", "exited").await;

    replay_to_head(&pool).await;

    assert_eq!(
        stamp(&pool, "retired").await,
        Some(1_788_566_400_000),
        "a legacy superseded queue must be retired by the upgrade, or the first restart after \
         it replays a sentence from days ago into a brand new thread"
    );
    for live in ["live-idle", "live-turn"] {
        assert_eq!(
            stamp(&pool, live).await,
            None,
            "{live} goes on living across the upgrade and will receive new input; stamping it \
             would make that new input unharvestable"
        );
    }
    for dead in ["dead-failed", "dead-exited"] {
        assert_eq!(
            stamp(&pool, dead).await,
            None,
            "{dead} is not `superseded`, and the harvest predicate never reads it either — the \
             backfill must not widen its own row set"
        );
    }
}
