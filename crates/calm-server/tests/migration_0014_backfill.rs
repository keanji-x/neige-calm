//! Issue #229 PR B — migration 0014 backfill smoke tests.
//!
//! Covers:
//!
//!   1. Migration runs cleanly on a fresh DB (no errors, no rows
//!      since the source table `waves` is empty).
//!   2. After inserting a wave + applying the migration's logic
//!      manually, the wave gets exactly one report card with the
//!      correct payload shape + `deletable = 0` + `role = 'reportcard'`.
//!   3. Re-running the migration's INSERT/UPDATE statements is a
//!      no-op — `WHERE NOT EXISTS` prevents duplicates.
//!   4. Layout overlay is seeded for waves that lacked one; for waves
//!      that already had a layout, the report card position is patched
//!      into the existing positions map.
//!
//! Why we replay the SQL manually rather than depending on sqlx to do
//! it: sqlx runs each migration exactly once per DB. Once the test
//! fixture's `SqlxRepo::open()` finishes, every migration (including
//! 0014) is marked applied. We can't ask sqlx to "run 0014 again"
//! against waves we minted *after* open. Replaying the bare SQL gives
//! us the same logical effect — and verifies the idempotency claim
//! (which is the operator-facing invariant: re-running this binary
//! on a DB that already saw 0014 must not double-mint).

#![cfg(unix)]

use std::sync::Arc;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::model::{NewCove, NewWave};
use calm_server::wave_report::WaveReportPayload;
use serde_json::Value;
use sqlx::SqlitePool;

/// The verbatim SQL from `migrations/0014_wave_report_card.sql`,
/// inlined here so we can replay it against rows minted *after* the
/// initial `SqlxRepo::open()` migration sweep. Keeping a single
/// constant means the test breaks loudly if the migration file
/// drifts — which is the right outcome (a behavioural change in the
/// migration should land alongside this test's update).
const MIGRATION_0014_SQL: &str = include_str!("../migrations/0014_wave_report_card.sql");

/// Apply the migration's statements directly against the live pool.
/// We strip comments first (so the split doesn't slice inside a
/// `-- foo;` line and produce a half-statement), then split on `;`
/// at top level. sqlite/sqlx's `query()` accepts only one statement
/// per call, so we feed them one at a time.
async fn replay_migration(pool: &SqlitePool) {
    // 1. Strip line comments. This is the key step: leaving comments
    //    in means a `; -- text\n` line would have the semicolon land
    //    inside a comment after splitting, which sqlite rejects.
    let stripped: String = MIGRATION_0014_SQL
        .lines()
        .map(|l| {
            // Find `--` outside any string. The migration's strings
            // ('# Goal', 'kernel', ...) don't contain `--`, so a naive
            // `find("--")` is safe here. If we ever need richer logic
            // we can borrow sqlite's own tokenizer.
            match l.find("--") {
                Some(idx) => &l[..idx],
                None => l,
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    // 2. Split on `;` at top level and execute each non-empty chunk.
    for raw in stripped.split(';') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        sqlx::query(trimmed)
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("replay failed on stmt:\n{trimmed}\nerror: {e}"));
    }
}

async fn fresh_repo() -> (Arc<dyn Repo>, SqlitePool) {
    let url = "sqlite::memory:";
    let repo = SqlxRepo::open(url).await.expect("open");
    // Reach into the inner pool for raw SQL replay below. `SqlxRepo`
    // exposes its pool via a doc(hidden) accessor used by the same
    // pattern in `tests/repo.rs`.
    let pool = repo.pool().clone();
    (Arc::new(repo), pool)
}

#[tokio::test]
async fn fresh_db_migration_is_no_op_when_no_waves() {
    // Open runs every migration including 0014 on an empty DB —
    // no rows to backfill, no errors.
    let (repo, _pool) = fresh_repo().await;
    let waves = repo
        .waves_by_cove("nonexistent")
        .await
        .expect("waves_by_cove works post-migration");
    assert!(waves.is_empty(), "no waves means no report cards");
}

#[tokio::test]
async fn backfill_mints_report_card_per_wave() {
    let (repo, pool) = fresh_repo().await;
    // Mint a cove + wave directly via the repo (bypassing the HTTP
    // route, so no report card is auto-minted). This simulates the
    // pre-0014 storage shape — wave row exists, no report card.
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "legacy".into(),
            sort: None,
        })
        .await
        .unwrap();
    // No cards yet under this wave.
    let cards = repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    assert_eq!(cards.len(), 0);

    // Replay the migration's SQL — same effect as upgrading a real DB.
    replay_migration(&pool).await;

    let cards = repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    assert_eq!(cards.len(), 1, "exactly one report card backfilled");
    let report = &cards[0];
    assert_eq!(report.kind, "wave-report");
    assert!(!report.deletable, "kernel-owned: deletable=false");
    // Sort places the report ahead of any other card.
    assert!(report.sort < 0.0, "sort < 0, got {}", report.sort);
    // Payload deserializes as the v1 initial shape — the body matches
    // `WaveReportPayload::initial()` (also asserted in
    // `wave_report::tests::initial_matches_migration_seed_body`).
    let payload: WaveReportPayload = serde_json::from_value(report.payload.clone())
        .expect("payload is a valid WaveReportPayload");
    assert_eq!(payload, WaveReportPayload::initial());
}

#[tokio::test]
async fn backfill_skips_waves_that_already_have_a_report_card() {
    let (repo, pool) = fresh_repo().await;
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "already migrated".into(),
            sort: None,
        })
        .await
        .unwrap();
    // First pass: mints a report card.
    replay_migration(&pool).await;
    let after_first = repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    assert_eq!(after_first.len(), 1);
    let first_report_id = after_first[0].id.clone();

    // Second pass: idempotent — no new rows, no error.
    replay_migration(&pool).await;
    let after_second = repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    assert_eq!(after_second.len(), 1, "no duplicate mint");
    assert_eq!(
        after_second[0].id, first_report_id,
        "same report card row — re-run was a no-op"
    );
}

#[tokio::test]
async fn backfill_seeds_layout_overlay_when_absent() {
    let (repo, pool) = fresh_repo().await;
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "no-layout-yet".into(),
            sort: None,
        })
        .await
        .unwrap();
    replay_migration(&pool).await;

    // The layout overlay now exists, with the report card pinned
    // at (0, 0, 12, 4).
    let overlays = repo.overlays_for("view", wave.id.as_str()).await.unwrap();
    let layout = overlays
        .iter()
        .find(|o| o.kind == "layout")
        .expect("layout overlay seeded");
    let positions = layout
        .payload
        .get("positions")
        .and_then(Value::as_object)
        .expect("payload.positions is an object");

    let cards = repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    let report_id = cards[0].id.as_str();
    let pos = positions
        .get(report_id)
        .and_then(Value::as_object)
        .expect("report card has a position entry");
    assert_eq!(pos.get("x").and_then(Value::as_i64), Some(0));
    assert_eq!(pos.get("y").and_then(Value::as_i64), Some(0));
    assert_eq!(pos.get("w").and_then(Value::as_i64), Some(12));
    assert_eq!(pos.get("h").and_then(Value::as_i64), Some(4));
}

#[tokio::test]
async fn backfill_patches_existing_layout_overlay() {
    let (repo, pool) = fresh_repo().await;
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "already-has-layout".into(),
            sort: None,
        })
        .await
        .unwrap();
    // Seed an existing layout overlay with one other card's position
    // already recorded.
    repo.overlay_upsert(calm_server::model::NewOverlay {
        plugin_id: "kernel".into(),
        entity_kind: "view".into(),
        entity_id: wave.id.as_str().to_string(),
        kind: "layout".into(),
        payload: serde_json::json!({
            "schemaVersion": 1,
            "positions": {
                "existing-card-id": { "x": 0, "y": 4, "w": 6, "h": 3 }
            }
        }),
    })
    .await
    .unwrap();

    replay_migration(&pool).await;

    // The overlay now carries BOTH the original entry AND the new
    // report card's position.
    let overlays = repo.overlays_for("view", wave.id.as_str()).await.unwrap();
    let layout = overlays
        .iter()
        .find(|o| o.kind == "layout")
        .expect("layout overlay present");
    let positions = layout
        .payload
        .get("positions")
        .and_then(Value::as_object)
        .expect("payload.positions is an object");
    assert!(
        positions.contains_key("existing-card-id"),
        "pre-existing position survives the patch: {positions:?}"
    );
    let cards = repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    let report_id = cards[0].id.as_str();
    assert!(
        positions.contains_key(report_id),
        "report card position added: {positions:?}"
    );

    // And it's idempotent — running again doesn't duplicate or churn.
    let layout_id_before = layout.id.clone();
    replay_migration(&pool).await;
    let overlays_after = repo.overlays_for("view", wave.id.as_str()).await.unwrap();
    let layout_after = overlays_after.iter().find(|o| o.kind == "layout").unwrap();
    assert_eq!(
        layout_after.id, layout_id_before,
        "same overlay row — second pass is a no-op"
    );
}
