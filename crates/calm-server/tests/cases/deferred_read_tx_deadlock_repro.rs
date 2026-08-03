//! Issue #1016 — FAILING REPRO for the `READ_ONLY_DEFERRED_ALLOWLIST`
//! justification in `tests/cases/deferred_write_tx_invariant.rs`.
//!
//! That allowlist exempts three deferred (`pool.begin()`) transactions on
//! the grounds that
//!
//!   > A deferred transaction that performs no writes never competes for
//!   > the shared-cache writer slot, so it cannot be a hold-and-wait party.
//!
//! The premise is wrong. Closing a shared-cache wait cycle does not require
//! competing for the *writer slot*; it only requires being a lock-HOLDING
//! waiter. `calm_truth::db::sqlite::deadlock_semantics_tests` already pins
//! the correct rule:
//!
//!   > The deadlock cycle requires a lock-HOLDING waiter — i.e. a reader
//!   > (or a deferred tx that acquired read locks before writing).
//!
//! A multi-table read-only deferred tx holds R locks on every table it has
//! already SELECTed while it parks on the next one. Those R locks block an
//! IMMEDIATE writer's W request just as effectively as a W lock would.
//!
//! This test drives the REAL production reader `Repo::wave_detail`
//! (`calm-truth/src/db/sqlite/read.rs`, allowlist entry
//! `async fn wave_detail(`) against the REAL production writer sequence of
//! `DELETE /api/waves/:id` (`calm-server/src/routes/waves.rs`):
//! `write_with_events_typed` (= `begin_immediate_tx`) →
//! `overlay_delete_card_overlays_by_wave_tx` →
//! `overlay_delete_by_entity_tx` ×2 → `wave_delete_tx`.
//!
//! (`release_workspace_leases_for_wave_tx` sits between the overlay deletes
//! and `wave_delete_tx` in the route but is `pub(crate)` to calm-server, so
//! it is not callable from an integration test. It only touches
//! `workspace_leases`, which neither side of this cycle reads, so its
//! absence cannot manufacture the deadlock.)
//!
//! Interleaving — pinned by LOCKS plus two oneshot channels, not by
//! sleep-and-hope:
//!
//!   1. writer: BEGIN IMMEDIATE, run the three overlay deletes → holds
//!      W(overlays); signals `overlays_locked`, then parks on `go`.
//!   2. test: spawns the reader `repo.wave_detail(id)`. It takes R(waves),
//!      R(cards), then MUST park on `overlays` — the writer holds W there.
//!      The reader is now a lock-holding waiter.
//!   3. test: releases `go`.
//!   4. writer: `wave_delete_tx` walks down to `DELETE FROM waves`, which
//!      needs W(waves) — held R by the parked reader. Cycle closed.
//!
//! The test asserts the invariant the allowlist CLAIMS to hold — that no
//! such cycle can form — so it is RED on today's tree and turns green only
//! when the gap is closed. Observed today: the writer's `wave_delete_tx`
//! fails with sqlite extended-result code **6** (`SQLITE_LOCKED`,
//! "database is deadlocked") — the non-retryable cycle abort that #930 set
//! out to eliminate, not the retryable code 5 (`SQLITE_BUSY`).
//!
//! Scope note: the app's in-memory sqlite (`db_url=mock`, CI and
//! `make dev-fresh`) is a shared-cache database with table-granularity
//! locks, which is exactly where this cycle lives.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, overlay_delete_by_entity_tx, overlay_delete_card_overlays_by_wave_tx, wave_delete_tx,
};
use calm_server::db::{RepoEventWrite, write_with_events_typed};
use calm_server::error::{CalmError, Result};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::{NewCard, NewCove, NewOverlay, NewWave};
use serde_json::json;
use tokio::sync::oneshot;

/// How long the reader is given to reach its park point on `overlays`
/// before the writer is released. This is a liveness margin only — it
/// cannot fabricate the result: if the reader had not yet taken R(waves),
/// `wave_delete_tx` would simply SUCCEED and the assertion below would fail
/// loudly rather than pass by luck.
const PARK_GRACE: Duration = Duration::from_secs(2);

/// Extended sqlite result code carried by a `CalmError::Db`, if any.
fn sqlite_code(err: &CalmError) -> Option<String> {
    match err {
        CalmError::Db(e) => e.as_database_error()?.code().map(|c| c.to_string()),
        _ => None,
    }
}

/// What the writer's `wave_delete_tx` produced, shipped out of the write
/// closure so the assertion can run on the test task.
#[derive(Debug)]
struct WriterOutcome {
    code: Option<String>,
    message: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_deferred_wave_detail_closes_a_deadlock_cycle_with_the_wave_delete_writer() {
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite (shared cache)"),
    );

    let cove = repo
        .cove_create(NewCove {
            name: "deadlock-repro".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("cove_create");
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: "deadlock repro".into(),
            sort: None,
            cwd: "/workspace".into(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("wave_create");
    let wave_id = wave.id.to_string();

    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "note".into(),
            sort: None,
            payload: json!({"text": "repro"}),
            title: Some("repro card".into()),
        })
        .await
        .expect("card_create");

    // Real overlay rows on both scopes the route deletes, so the writer's
    // overlay statements are not no-ops.
    repo.overlay_upsert(NewOverlay {
        plugin_id: "repro".into(),
        entity_kind: "wave".into(),
        entity_id: wave_id.clone(),
        kind: "badge".into(),
        payload: json!({"n": 1}),
    })
    .await
    .expect("wave overlay");
    repo.overlay_upsert(NewOverlay {
        plugin_id: "repro".into(),
        entity_kind: "card".into(),
        entity_id: card.id.to_string(),
        kind: "badge".into(),
        payload: json!({"n": 2}),
    })
    .await
    .expect("card overlay");

    let bus = EventBus::new();
    let role_cache = CardRoleCache::new();
    let cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&cove_cache)
        .await
        .expect("seed wave->cove cache");

    let (overlays_locked_tx, overlays_locked_rx) = oneshot::channel::<()>();
    let (go_tx, go_rx) = oneshot::channel::<()>();
    let (outcome_tx, outcome_rx) = oneshot::channel::<WriterOutcome>();

    // ---- writer: the DELETE /api/waves/:id transaction, verbatim --------
    let repo_w = Arc::clone(&repo);
    let wave_id_w = wave_id.clone();
    let cove_cache_w = cove_cache.clone();
    let write_ctx = calm_server::state::WriteContext::new(role_cache.clone(), cove_cache.clone());
    let writer = tokio::spawn(async move {
        write_with_events_typed(
            repo_w.as_ref() as &dyn RepoEventWrite,
            ActorId::User,
            None,
            &bus,
            &write_ctx,
            move |tx| {
                Box::pin(async move {
                    overlay_delete_card_overlays_by_wave_tx(tx, &wave_id_w).await?;
                    overlay_delete_by_entity_tx(tx, "wave", &wave_id_w).await?;
                    overlay_delete_by_entity_tx(tx, "view", &wave_id_w).await?;

                    // W(overlays) is now held by this IMMEDIATE tx.
                    overlays_locked_tx
                        .send(())
                        .expect("test task must still be listening");
                    go_rx.await.expect("test task must release the writer");

                    let res = wave_delete_tx(tx, &wave_id_w, &cove_cache_w)
                        .await
                        .map_err(CalmError::from);
                    let outcome = match &res {
                        Ok(()) => WriterOutcome {
                            code: None,
                            message: "wave_delete_tx succeeded (no cycle)".into(),
                        },
                        Err(e) => WriterOutcome {
                            code: sqlite_code(e),
                            message: format!("{e}"),
                        },
                    };
                    let _ = outcome_tx.send(outcome);

                    // Always abort: this repro must never commit, and the
                    // rollback is the production error path that unparks
                    // the reader.
                    let out: Result<((), Vec<(EventScope, Event)>)> = Err(CalmError::Internal(
                        "deadlock repro: transaction intentionally rolled back".into(),
                    ));
                    out
                })
            },
        )
        .await
    });

    // ---- reader: the REAL allowlisted deferred read tx -------------------
    overlays_locked_rx
        .await
        .expect("writer must reach the overlay-locked seam");

    let repo_r = Arc::clone(&repo);
    let wave_id_r = wave_id.clone();
    let reader = tokio::spawn(async move { repo_r.wave_detail(&wave_id_r).await });

    tokio::time::sleep(PARK_GRACE).await;
    assert!(
        !reader.is_finished(),
        "wave_detail must be PARKED on overlays (W-held by the writer) while \
         still holding R(waves)+R(cards) — that is the hold-and-wait state \
         the allowlist claims cannot exist"
    );

    go_tx.send(()).expect("writer must still be parked on go");

    let outcome = tokio::time::timeout(Duration::from_secs(30), outcome_rx)
        .await
        .expect("writer must not stall forever")
        .expect("writer must report its wave_delete_tx outcome");

    eprintln!(
        "[#1016 repro] writer wave_delete_tx -> code={:?} message={}",
        outcome.code, outcome.message
    );

    // THE INVARIANT UNDER TEST (currently violated — this is the failing
    // repro for #1016). The allowlist in `deferred_write_tx_invariant.rs`
    // asserts that a read-only deferred tx "cannot be a hold-and-wait
    // party". If that were true, no interleaving of `wave_detail` with the
    // wave-delete writer could ever produce sqlite's non-retryable cycle
    // abort, `SQLITE_LOCKED` (6). Code 5 (`SQLITE_BUSY`) would be fine —
    // that one is retryable and is not what #930 set out to kill.
    assert_ne!(
        outcome.code.as_deref(),
        Some("6"),
        "#1016: the ALLOWLISTED read-only deferred transaction \
         `read.rs::wave_detail` acted as a lock-HOLDING waiter \
         (R(waves)+R(cards) held while parked on overlays) and closed a \
         deadlock cycle with the real `DELETE /api/waves/:id` writer. \
         `wave_delete_tx` aborted with SQLITE_LOCKED (6) \"database is \
         deadlocked\" — the non-retryable production symptom #930 set out \
         to eliminate. The allowlist justification (\"a deferred \
         transaction that performs no writes ... cannot be a hold-and-wait \
         party\") is therefore false for multi-table readers. \
         Observed: {outcome:?}"
    );

    // Housekeeping: let both sides finish so the runtime shuts down clean.
    let _ = tokio::time::timeout(Duration::from_secs(30), writer).await;
    let _ = tokio::time::timeout(Duration::from_secs(30), reader).await;
}
