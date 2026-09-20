//! A multi-table read-only deferred tx holds R locks while parked, so it can close a
//! shared-cache deadlock cycle (`SQLITE_LOCKED`, code 6) against an IMMEDIATE writer; the
//! production reader `track_detail` must therefore be a single autocommit statement.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, overlay_delete_by_entity_tx, overlay_delete_card_overlays_by_track_tx,
    track_delete_tx,
};
use calm_server::db::{RepoEventWrite, write_with_events_typed};
use calm_server::error::{CalmError, Result};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::{NewArea, NewCard, NewOverlay, NewTrack};
use serde_json::json;
use tokio::sync::oneshot;

/// How much longer than an UNCONTENDED `track_detail` the reader must stay unfinished before
/// the writer is released; a ratio against a measured baseline, not a wall-clock constant.
const PARK_FLOOR_RATIO: u32 = 50;

/// Floor under `PARK_FLOOR_RATIO × baseline`, for the case where the
/// baseline read is so fast that 50× is still microseconds.
const MIN_PARK_FLOOR: Duration = Duration::from_millis(250);

/// Hard cap on waiting for the "reader holds a checked-out connection" observation;
/// reaching it is a failure, not a fallback.
const CHECKOUT_OBSERVE_CAP: Duration = Duration::from_secs(30);

/// Connections in use right now. `num_idle()` is approximate and may transiently exceed
/// `size()`, so the subtraction saturates (which can only under-report).
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

/// What the writer's `track_delete_tx` produced, shipped out of the write
/// closure so the assertion can run on the test task.
#[derive(Debug)]
struct WriterOutcome {
    code: Option<String>,
    message: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_deferred_track_detail_closes_a_deadlock_cycle_with_the_track_delete_writer() {
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite (shared cache)"),
    );

    let area = repo
        .area_create(NewArea {
            name: "deadlock-repro".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("area_create");
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "deadlock repro".into(),
            sort: None,
            cwd: "/workspace".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("track_create");
    let track_id = track.id.to_string();

    let card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
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
        entity_kind: "track".into(),
        entity_id: track_id.clone(),
        kind: "badge".into(),
        payload: json!({"n": 1}),
    })
    .await
    .expect("track overlay");
    repo.overlay_upsert(NewOverlay {
        plugin_id: "repro".into(),
        entity_kind: "card".into(),
        entity_id: card.id.to_string(),
        kind: "badge".into(),
        payload: json!({"n": 2}),
    })
    .await
    .expect("card overlay");

    // Baseline: what this `track_detail` costs with nobody holding a lock, so "the reader is
    // just slow" is quantified away.
    let mut baseline = Duration::ZERO;
    for _ in 0..5 {
        let started = std::time::Instant::now();
        repo.track_detail(&track_id)
            .await
            .expect("baseline track_detail")
            .expect("track exists");
        baseline = baseline.max(started.elapsed());
    }
    let park_floor = (baseline * PARK_FLOOR_RATIO).max(MIN_PARK_FLOOR);

    let bus = EventBus::new();
    let role_cache = CardRoleCache::new();
    let area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&area_cache)
        .await
        .expect("seed track->area cache");

    let (overlays_locked_tx, overlays_locked_rx) = oneshot::channel::<()>();
    let (go_tx, go_rx) = oneshot::channel::<()>();
    let (outcome_tx, outcome_rx) = oneshot::channel::<WriterOutcome>();

    // writer: the DELETE /api/tracks/:id transaction, verbatim
    let repo_w = Arc::clone(&repo);
    let track_id_w = track_id.clone();
    let area_cache_w = area_cache.clone();
    let write_ctx = calm_server::state::WriteContext::new(role_cache.clone(), area_cache.clone());
    let writer = tokio::spawn(async move {
        write_with_events_typed(
            repo_w.as_ref() as &dyn RepoEventWrite,
            ActorId::User,
            None,
            &bus,
            &write_ctx,
            move |tx| {
                Box::pin(async move {
                    overlay_delete_card_overlays_by_track_tx(tx, &track_id_w).await?;
                    overlay_delete_by_entity_tx(tx, "track", &track_id_w).await?;
                    overlay_delete_by_entity_tx(tx, "view", &track_id_w).await?;

                    // W(overlays) is now held by this IMMEDIATE tx.
                    overlays_locked_tx
                        .send(())
                        .expect("test task must still be listening");
                    go_rx.await.expect("test task must release the writer");

                    let res = track_delete_tx(tx, &track_id_w, &area_cache_w)
                        .await
                        .map_err(CalmError::from);
                    let outcome = match &res {
                        Ok(()) => WriterOutcome {
                            code: None,
                            message: "track_delete_tx succeeded (no cycle)".into(),
                        },
                        Err(e) => WriterOutcome {
                            code: sqlite_code(e),
                            message: format!("{e}"),
                        },
                    };
                    let _ = outcome_tx.send(outcome);

                    // Always abort: the rollback is the production error path that unparks the reader.
                    let out: Result<((), Vec<(EventScope, Event)>)> = Err(CalmError::Internal(
                        "deadlock repro: transaction intentionally rolled back".into(),
                    ));
                    out
                })
            },
        )
        .await
    });

    // reader: the production `track_detail`
    overlays_locked_rx
        .await
        .expect("writer must reach the overlay-locked seam");

    let repo_r = Arc::clone(&repo);
    let track_id_r = track_id.clone();
    let (entered_tx, entered_rx) = oneshot::channel::<()>();
    let reader = tokio::spawn(async move {
        // The reader task was actually polled; without this `!is_finished()` is vacuous.
        entered_tx.send(()).expect("test task must be listening");
        let out = repo_r.track_detail(&track_id_r).await;
        (out, std::time::Instant::now())
    });
    tokio::time::timeout(CHECKOUT_OBSERVE_CAP, entered_rx)
        .await
        .expect("reader must reach track_detail")
        .expect("reader task must not be dropped");
    let entered_at = std::time::Instant::now();

    // Wait (not sample) for the reader to hold a checked-out connection, then require it to
    // stay unfinished for `park_floor` — the difference between "blocked" and "slow".
    let pool = repo.pool();
    let mut peak_in_use = 0usize;
    let mut checked_out_at = None;
    loop {
        tokio::task::yield_now().await;
        assert!(
            !reader.is_finished(),
            "track_detail must be PARKED on `overlays` (W-held by the writer). \
             It entered the read while the writer already held W(overlays), so \
             finishing early would mean it never took the lock at all"
        );
        peak_in_use = peak_in_use.max(connections_in_use(pool));
        if checked_out_at.is_none() && peak_in_use >= 2 {
            checked_out_at = Some(std::time::Instant::now());
        }
        match checked_out_at {
            // Connection observed — hold for the calibrated park floor.
            Some(seen_at) if seen_at.elapsed() >= park_floor => break,
            Some(_) => {}
            // Still waiting for the connection checkout.
            None => assert!(
                entered_at.elapsed() < CHECKOUT_OBSERVE_CAP,
                "the reader never held a checked-out sqlite connection \
                 (writer + reader = 2 in use) within {CHECKOUT_OBSERVE_CAP:?} \
                 of entering `track_detail`; peak observed {peak_in_use}. That \
                 means it never got as far as issuing its statement, which \
                 would make the `code != 6` assertion below vacuous"
            ),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    eprintln!(
        "[#1016 repro] uncontended track_detail baseline={baseline:?}, \
         park floor={park_floor:?}, peak connections in use={peak_in_use}"
    );

    let released_at = std::time::Instant::now();
    go_tx.send(()).expect("writer must still be parked on go");

    let outcome = tokio::time::timeout(Duration::from_secs(30), outcome_rx)
        .await
        .expect("writer must not stall forever")
        .expect("writer must report its track_delete_tx outcome");

    eprintln!(
        "[#1016 repro] writer track_delete_tx -> code={:?} message={}",
        outcome.code, outcome.message
    );

    // Code 5 (`SQLITE_BUSY`) is retryable and fine; code 6 (`SQLITE_LOCKED`) is the cycle abort.
    assert_ne!(
        outcome.code.as_deref(),
        Some("6"),
        "#1016: the ALLOWLISTED read-only deferred transaction \
         `read.rs::track_detail` acted as a lock-HOLDING waiter \
         (R(tracks)+R(cards) held while parked on overlays) and closed a \
         deadlock cycle with the real `DELETE /api/tracks/:id` writer. \
         `track_delete_tx` aborted with SQLITE_LOCKED (6) \"database is \
         deadlocked\" — the non-retryable production symptom #930 set out \
         to eliminate. The allowlist justification (\"a deferred \
         transaction that performs no writes ... cannot be a hold-and-wait \
         party\") is therefore false for multi-table readers. \
         Observed: {outcome:?}"
    );

    // The writer DELETEd both overlay rows uncommitted, then rolled back, so a reader that
    // observes both rows can only have read `overlays` after being serialized behind the lock.
    let (detail, finished_at) = tokio::time::timeout(Duration::from_secs(30), reader)
        .await
        .expect("reader must not stall forever")
        .expect("reader task must not panic");
    let detail = detail
        .expect("track_detail must succeed")
        .expect("track must still exist — the writer rolled back");
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
        "reader must observe the track's card in the same snapshot"
    );
    assert!(
        finished_at > released_at,
        "reader must have completed only after the writer was released; \
         completing earlier would mean it never contended for `overlays`"
    );
    // The reader spent orders of magnitude longer inside `track_detail` than the uncontended baseline.
    let reader_wall = finished_at.duration_since(entered_at);
    assert!(
        reader_wall >= park_floor,
        "reader spent {reader_wall:?} in `track_detail` but an uncontended \
         read of the same track costs {baseline:?}; it must have been BLOCKED \
         for at least {park_floor:?} ({PARK_FLOOR_RATIO}× baseline). A \
         shorter stay means it was never actually waiting on the writer's \
         W(overlays)"
    );

    let _ = tokio::time::timeout(Duration::from_secs(30), writer).await;
}
