//! Issue #1016 — regression guard, originally the FAILING REPRO for the
//! `READ_ONLY_DEFERRED_ALLOWLIST` justification in
//! `tests/cases/deferred_write_tx_invariant.rs`.
//!
//! That allowlist used to exempt three deferred (`pool.begin()`)
//! transactions on the grounds that
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
//! `overlay_delete_by_entity_tx` ×2 → `wave_delete_tx`. Slow process/socket
//! teardown happens before this writer tx.
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
//!      Under the pre-fix deferred tx the reader is at this point a
//!      lock-HOLDING waiter; under the fix it has already released
//!      everything.
//!   3. test: releases `go`.
//!   4. writer: `wave_delete_tx` walks down to `DELETE FROM waves`, which
//!      needs W(waves) — held R by the parked reader in the pre-fix shape.
//!      Cycle closed, code 6.
//!
//! The test asserts the invariant the allowlist CLAIMED to hold — that no
//! such cycle can form. It was RED when written (the writer's
//! `wave_delete_tx` failed with sqlite extended-result code **6**,
//! `SQLITE_LOCKED` "database is deadlocked" — the non-retryable cycle
//! abort #930 set out to eliminate, not the retryable code 5
//! `SQLITE_BUSY`). The fix dropped the explicit transaction from
//! `wave_detail` and collapsed its three SELECTs into ONE statement: a
//! single statement is AUTOCOMMIT (a blocked autocommit statement unwinds
//! its implicit transaction, releasing every table lock it took, before
//! parking in `unlock_notify` — so it can never be the lock-HOLDING
//! waiter) and is simultaneously one implicit transaction (so the read
//! keeps its snapshot). The reader still PARKS on `overlays`, it just
//! holds nothing while parked, so the writer walks through.
//!
//! Non-vacuity. `!reader.is_finished()` alone proves nothing — it holds
//! for a task the runtime has not polled yet, and the headline assertion
//! is a NEGATIVE one (`code != 6`) that a never-scheduled reader would
//! satisfy trivially. Four positive facts pin the reader to its park
//! point instead:
//!   1. it signals `entered` from inside the spawned task before calling
//!      `wave_detail`, and the test awaits that signal BEFORE releasing
//!      the writer. That rules out "never polled";
//!   2. it HOLDS a checked-out sqlite connection while the writer is still
//!      parked — the pool shows two connections in use, writer plus
//!      reader. The test does not sample-and-hope for this: it WAITS for
//!      the observation and fails if it never arrives. A reader still
//!      sitting in `pool.acquire()` holds none;
//!   3. it stays unfinished for at least `PARK_FLOOR_RATIO` times the
//!      measured uncontended `wave_detail` latency (floor
//!      `MIN_PARK_FLOOR`). The baseline is taken on this same wave, on
//!      this machine, before the writer takes any lock — so "the reader
//!      is merely slow" is quantified away rather than assumed, and a
//!      slow machine scales the bar instead of flaking;
//!   4. it comes back with BOTH overlay rows. The writer had deleted them
//!      (uncommitted) before the reader started and only restored them by
//!      rolling back, so observing them is proof the reader's `overlays`
//!      read landed after the writer released the lock — it queued, it did
//!      not skip.
//!
//! Honest limit of that evidence. (2) proves the reader owns a connection,
//! not that it has already handed a statement to sqlite; nothing in
//! userspace can distinguish those two, precisely BECAUSE the fix makes a
//! parked reader hold no table locks and therefore leave no observable
//! trace. What closes the gap is (4) combined with (3): the reader was
//! inside `wave_detail` for two orders of magnitude longer than the read
//! costs uncontended, holding a connection the whole time, and the rows it
//! returned exist only on the far side of the writer's rollback. The one
//! interleaving that would make `code != 6` vacuous — a reader that never
//! touched `overlays` while the writer ran — is excluded by (4) alone.
//!
//! Sibling reader. `read.rs::task_diagnostics` took the same route and has
//! no dynamic repro of its own, deliberately. What a repro can establish is
//! the MECHANISM — that a deferred tx really closes a cycle and that
//! autocommit really does not — and that is call-site independent; it is
//! pinned here and in `calm_truth::db::sqlite::deadlock_semantics_tests`.
//! What guards the call sites is
//! `deferred_write_tx_invariant::production_deferred_transactions_are_read_only_allowlisted`,
//! whose allowlist is now EMPTY: any `.begin(` reappearing anywhere under
//! `calm-server/src` or `calm-truth/src` fails it. That is a universal
//! fail-closed negative over both readers, strictly wider than a
//! per-call-site test could be.
//!
//! Scope note: the app's in-memory sqlite (`db_url=mock`, CI and
//! `make dev-fresh`) is a shared-cache database with table-granularity
//! locks, which is exactly where this cycle lives. The production file
//! database (PRIVATECACHE + WAL) hands readers an MVCC snapshot and never
//! forms this cycle at all — which is why the fix had to be one that costs
//! production nothing.

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

/// How much longer than an UNCONTENDED `wave_detail` the reader must stay
/// unfinished before the writer is released.
///
/// Deliberately a ratio against a baseline measured on this wave, on this
/// machine, moments earlier — not a wall-clock constant. A fixed window is
/// the classic CI flake: too short on a loaded box and the test fails for
/// scheduling reasons, too long and it wastes CI time everywhere. A ratio
/// also states the actual claim: the reader is not slow, it is BLOCKED.
const PARK_FLOOR_RATIO: u32 = 50;

/// Floor under `PARK_FLOOR_RATIO × baseline`, for the case where the
/// baseline read is so fast that 50× is still microseconds.
const MIN_PARK_FLOOR: Duration = Duration::from_millis(250);

/// Hard cap on waiting for the "reader holds a checked-out connection"
/// observation. Reaching it is a FAILURE, not a fallback: it means the
/// reader never got a connection, which would make the headline `code != 6`
/// assertion vacuous. Generous, because overshooting costs nothing on a
/// healthy machine (the observation normally lands in milliseconds) while a
/// tight value is exactly what turns a slow box into a red build.
const CHECKOUT_OBSERVE_CAP: Duration = Duration::from_secs(30);

/// Connections in use right now, per the pool's own counters.
///
/// `num_idle()` is documented as APPROXIMATE and may transiently exceed
/// `size()`, so the subtraction saturates — an unsigned underflow here would
/// panic the test in debug builds for reasons that have nothing to do with
/// the invariant under test. Saturating can only UNDER-report, which makes
/// the caller wait one more iteration rather than pass wrongly.
fn connections_in_use(pool: &sqlx::SqlitePool) -> usize {
    (pool.size() as usize).saturating_sub(pool.num_idle())
}

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
            plugin_scope: None,
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

    // Baseline: what does this exact `wave_detail` cost with NOBODY holding
    // a lock, on this machine, right now? Evidence #3 below is expressed as
    // a multiple of this, so "the reader is just slow" stops being a
    // hand-wave and a loaded CI box raises the bar instead of flaking.
    let mut baseline = Duration::ZERO;
    for _ in 0..5 {
        let started = std::time::Instant::now();
        repo.wave_detail(&wave_id)
            .await
            .expect("baseline wave_detail")
            .expect("wave exists");
        baseline = baseline.max(started.elapsed());
    }
    let park_floor = (baseline * PARK_FLOOR_RATIO).max(MIN_PARK_FLOOR);

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
    let (entered_tx, entered_rx) = oneshot::channel::<()>();
    let reader = tokio::spawn(async move {
        // Positive evidence #1: the reader task was actually polled and is
        // inside `wave_detail`. Without this the `!is_finished()` check
        // below is vacuous — it holds just as well for a task the runtime
        // has not touched yet.
        entered_tx.send(()).expect("test task must be listening");
        let out = repo_r.wave_detail(&wave_id_r).await;
        (out, std::time::Instant::now())
    });
    tokio::time::timeout(CHECKOUT_OBSERVE_CAP, entered_rx)
        .await
        .expect("reader must reach wave_detail")
        .expect("reader task must not be dropped");
    let entered_at = std::time::Instant::now();

    // Positive evidence #2: the reader holds a checked-out sqlite connection
    // while the writer is still parked, so the pool shows two in use (writer
    // + reader). WAITED FOR, not sampled: a reader still inside
    // `pool.acquire()` simply has not produced the evidence yet, and giving
    // up early would be the flake. Blowing the cap is a hard failure.
    //
    // Positive evidence #3: the reader must then stay unfinished for
    // `park_floor` — 50× the uncontended baseline measured above — while
    // being handed scheduling opportunities on every iteration. That is the
    // difference between "blocked" and "slow", stated in units this machine
    // just calibrated.
    let pool = repo.pool();
    let mut peak_in_use = 0usize;
    let mut checked_out_at = None;
    loop {
        tokio::task::yield_now().await;
        assert!(
            !reader.is_finished(),
            "wave_detail must be PARKED on `overlays` (W-held by the writer). \
             It entered the read while the writer already held W(overlays), so \
             finishing early would mean it never took the lock at all"
        );
        peak_in_use = peak_in_use.max(connections_in_use(pool));
        if checked_out_at.is_none() && peak_in_use >= 2 {
            checked_out_at = Some(std::time::Instant::now());
        }
        match checked_out_at {
            // Evidence #2 in hand — hold for the calibrated park floor.
            Some(seen_at) if seen_at.elapsed() >= park_floor => break,
            Some(_) => {}
            // Still waiting for evidence #2.
            None => assert!(
                entered_at.elapsed() < CHECKOUT_OBSERVE_CAP,
                "the reader never held a checked-out sqlite connection \
                 (writer + reader = 2 in use) within {CHECKOUT_OBSERVE_CAP:?} \
                 of entering `wave_detail`; peak observed {peak_in_use}. That \
                 means it never got as far as issuing its statement, which \
                 would make the `code != 6` assertion below vacuous"
            ),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    eprintln!(
        "[#1016 repro] uncontended wave_detail baseline={baseline:?}, \
         park floor={park_floor:?}, peak connections in use={peak_in_use}"
    );

    let released_at = std::time::Instant::now();
    go_tx.send(()).expect("writer must still be parked on go");

    let outcome = tokio::time::timeout(Duration::from_secs(30), outcome_rx)
        .await
        .expect("writer must not stall forever")
        .expect("writer must report its wave_delete_tx outcome");

    eprintln!(
        "[#1016 repro] writer wave_delete_tx -> code={:?} message={}",
        outcome.code, outcome.message
    );

    // THE INVARIANT UNDER TEST (violated when this repro was written; #1016
    // closed the gap by removing the explicit tx). The allowlist in
    // `deferred_write_tx_invariant.rs` asserted that a read-only deferred tx
    // "cannot be a hold-and-wait party". If that were true, no interleaving of `wave_detail` with the
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

    // ---- positive evidence that the reader really went through `overlays`
    //
    // The reader entered `wave_detail` while the writer's IMMEDIATE tx held
    // W(overlays) with both seeded overlay rows DELETEd but not committed
    // (the route issues three overlay DELETE statements — card-scoped,
    // wave-scoped, view-scoped — but only two rows exist to be hit; the
    // "view" delete is a legitimate no-op, and adding a third row would not
    // strengthen anything the cycle depends on). The writer then rolls back,
    // restoring them. So a reader that observes both overlay rows can only
    // have read `overlays` AFTER that rollback — i.e. it was serialized
    // behind the writer's lock. A reader that was never scheduled, or that
    // somehow read through the lock, cannot produce this result: it would
    // have to see 0 overlays (uncommitted delete visible) or never reach the
    // assertion at all.
    //
    // This is evidence #4, and it is what makes the `assert_ne!(code, "6")`
    // above non-vacuous: the writer succeeded *while a live reader was
    // parked on its lock*, not because the reader had wandered off.
    let (detail, finished_at) = tokio::time::timeout(Duration::from_secs(30), reader)
        .await
        .expect("reader must not stall forever")
        .expect("reader task must not panic");
    let detail = detail
        .expect("wave_detail must succeed")
        .expect("wave must still exist — the writer rolled back");
    assert_eq!(
        detail.overlays.len(),
        2,
        "reader must observe both overlay rows, which only exist again after \
         the writer's rollback — that is the proof it PARKED on `overlays` \
         rather than never running. Observed: {:?}",
        detail.overlays
    );
    assert_eq!(
        detail.cards.len(),
        1,
        "reader must observe the wave's card in the same snapshot"
    );
    assert!(
        finished_at > released_at,
        "reader must have completed only after the writer was released; \
         completing earlier would mean it never contended for `overlays`"
    );
    // Evidence #3, cashed out: the reader spent orders of magnitude longer
    // inside `wave_detail` than the same read costs uncontended on this
    // machine. `park_floor` is derived from the baseline measured above, so
    // this scales with the box instead of encoding a wall-clock guess.
    let reader_wall = finished_at.duration_since(entered_at);
    assert!(
        reader_wall >= park_floor,
        "reader spent {reader_wall:?} in `wave_detail` but an uncontended \
         read of the same wave costs {baseline:?}; it must have been BLOCKED \
         for at least {park_floor:?} ({PARK_FLOOR_RATIO}× baseline). A \
         shorter stay means it was never actually waiting on the writer's \
         W(overlays)"
    );

    // Housekeeping: let the writer finish so the runtime shuts down clean.
    let _ = tokio::time::timeout(Duration::from_secs(30), writer).await;
}
