//! #1628 S2 — in-flight keys and lanes, with the resolver STARTED (real
//! drain tasks): design §6 A9, A9g, A9i, A9d, A9f.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use calm_server::report_series::Enqueue;
use serde_json::json;
use tokio::time::sleep;

use crate::report_series_fixture::{
    FixtureOptions, MARKET_PLUGIN_ID, SOURCE, SeriesFixture, ok_series, seam_fixture, wait_until,
};

fn started(timeout: Duration) -> FixtureOptions {
    FixtureOptions {
        unstarted: false,
        resolve_timeout: timeout,
        ..FixtureOptions::default()
    }
}

// ---------------------------------------------------------------------------
// A9 — ten enqueues of one key, one job
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_is_deduplicated_per_key() {
    let fx = SeriesFixture::boot(started(Duration::from_millis(400))).await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    // The plugin holds the first call for the whole timeout, so the key stays
    // in flight while the other nine enqueues arrive.
    fx.reply_hang();
    let mut outcomes = Vec::new();
    for _ in 0..10 {
        outcomes.push(fx.enqueue(&block_id).await);
    }
    assert_eq!(outcomes[0], Enqueue::Queued);
    assert!(
        outcomes[1..].iter().all(|o| *o == Enqueue::InFlight),
        "{outcomes:?}"
    );
    assert_eq!(fx.resolver().inflight_len(), 1);
    assert!(fx.resolver().lane_queue_len(MARKET_PLUGIN_ID) <= 1);
    fx.wait_for_row(&block_id, Duration::from_secs(5)).await;
    sleep(Duration::from_millis(150)).await;
    assert_eq!(fx.call_count(), 1, "one job, one call");
}

// ---------------------------------------------------------------------------
// A9g — a block rewritten while its job waits keeps ONE queued job, and the
// job resolves the payload current at dequeue time
// ---------------------------------------------------------------------------

struct RewriteRun {
    fx: SeriesFixture,
    versions: Vec<Vec<&'static str>>,
    outcomes: Vec<Enqueue>,
    inflight_while_queued: usize,
    queue_while_queued: usize,
}

async fn rewrite_five_times_behind_a_hung_call() -> RewriteRun {
    let fx = SeriesFixture::boot(started(Duration::from_millis(800))).await;
    // Block X occupies the lane: the plugin never answers its call, so the
    // lane is busy until the 800ms timeout.
    let x = fx
        .write_series_block(json!({ "source": SOURCE, "series": ["US:X"], "as_of": "2026-09-10" }))
        .await;
    let y = fx
        .write_series_block(json!({ "source": SOURCE, "series": ["US:V1"], "as_of": "2026-09-10" }))
        .await;
    fx.program(json!({ "mode": "sequence", "replies": [
        { "mode": "hang" },
        { "mode": "structured", "structured": { "series": [
            ok_series("US:V5", "2026-09-11", &[("2026-08-11", 1.0), ("2026-09-10", 2.0)])
        ]}},
    ]}));
    assert_eq!(fx.enqueue(&x).await, Enqueue::Queued);
    fx.wait_for_calls(1, Duration::from_secs(5)).await;

    let versions: Vec<Vec<&'static str>> = vec![
        vec!["US:V1"],
        vec!["US:V2"],
        vec!["US:V3"],
        vec!["US:V4"],
        vec!["US:V5"],
    ];
    let mut outcomes = Vec::new();
    outcomes.push(fx.enqueue(&y).await);
    for series in &versions[1..] {
        fx.rewrite_series_block(
            &y,
            json!({ "source": SOURCE, "series": series, "as_of": "2026-09-10" }),
        )
        .await;
        outcomes.push(fx.enqueue(&y).await);
    }
    let inflight_while_queued = fx.resolver().inflight_len();
    let queue_while_queued = fx.resolver().lane_queue_len(MARKET_PLUGIN_ID);
    // X times out, Y dequeues and is answered.
    fx.wait_for_calls(2, Duration::from_secs(5)).await;
    fx.wait_for_row(&y, Duration::from_secs(5)).await;
    RewriteRun {
        fx,
        versions,
        outcomes,
        inflight_while_queued,
        queue_while_queued,
    }
}

#[tokio::test]
async fn rewritten_block_keeps_one_queued_job() {
    let run = rewrite_five_times_behind_a_hung_call().await;
    assert_eq!(run.outcomes[0], Enqueue::Queued);
    assert!(
        run.outcomes[1..].iter().all(|o| *o == Enqueue::InFlight),
        "{:?}",
        run.outcomes
    );
    assert_eq!(
        run.inflight_while_queued, 2,
        "X running + Y queued: the key is (track, block), not (track, block, hash)"
    );
    assert_eq!(
        run.queue_while_queued, 1,
        "one queued job for five rewrites"
    );
    assert_eq!(run.fx.call_count(), 2);
    assert_eq!(run.versions.len(), 5);
}

#[tokio::test]
async fn dequeued_job_resolves_current_payload() {
    let run = rewrite_five_times_behind_a_hung_call().await;
    let calls = run.fx.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1]["arguments"]["series"],
        json!(run.versions[4]),
        "the plugin saw the fifth version, not the first"
    );
    let rows = run.fx.rows().await;
    let y_rows: Vec<_> = rows.iter().filter(|r| r.status == "ok").collect();
    assert_eq!(y_rows.len(), 1, "{rows:?}");
    let current = run
        .fx
        .current_request(
            &rows
                .iter()
                .find(|r| r.status == "ok")
                .map(|r| r.block_id.clone())
                .unwrap(),
        )
        .await;
    assert_eq!(
        y_rows[0].request_hash, current.request_hash,
        "the row carries the fifth version's hash"
    );
}

// ---------------------------------------------------------------------------
// A9i — check-and-insert is one step: a reader held inside the pre-check
// already owns the key
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_enqueue_admits_exactly_one() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    let request = fx.current_request(&block_id).await;
    fx.resolver().failpoints.hold_in_precheck();

    let resolver_a = fx.resolver().clone();
    let ctx_a = fx.ctx().clone();
    let track_a = fx.track_id().to_string();
    let block_a = block_id.clone();
    let request_a = request.clone();
    let reader_a = tokio::spawn(async move {
        resolver_a
            .enqueue(&ctx_a, &track_a, &block_a, &request_a)
            .await
    });
    let failpoints = &fx.resolver().failpoints;
    wait_until(
        "reader A inside the pre-check",
        Duration::from_secs(5),
        || failpoints.precheck_entered() == 1,
    )
    .await;

    let outcome_b = fx.enqueue(&block_id).await;
    assert_eq!(outcome_b, Enqueue::InFlight, "B sees A's key");
    fx.resolver().failpoints.release_precheck();
    let outcome_a = reader_a.await.expect("reader A");
    assert_eq!(outcome_a, Enqueue::Queued);
    assert_eq!(fx.resolver().inflight_len(), 1);
    assert_eq!(
        fx.resolver().take_recorded_jobs().len(),
        1,
        "exactly one job"
    );
}

// ---------------------------------------------------------------------------
// A9d — a panicked drain task is replaced on the next enqueue
// ---------------------------------------------------------------------------

#[tokio::test]
async fn panicked_lane_is_rebuilt() {
    let fx = SeriesFixture::boot(started(Duration::from_secs(5))).await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    fx.reply_structured(seam_fixture()["reply"].clone());
    fx.resolver().failpoints.panic_drain_once();
    assert_eq!(fx.enqueue(&block_id).await, Enqueue::Queued);
    let resolver = fx.resolver().clone();
    wait_until("the lane to die", Duration::from_secs(5), || {
        resolver.lane_is_finished(MARKET_PLUGIN_ID) == Some(true)
    })
    .await;
    assert_eq!(
        fx.resolver().inflight_len(),
        0,
        "the dropped job released its key"
    );
    assert_eq!(fx.call_count(), 0);

    assert_eq!(fx.enqueue(&block_id).await, Enqueue::Queued);
    fx.wait_for_calls(1, Duration::from_secs(5)).await;
    let row = fx.wait_for_row(&block_id, Duration::from_secs(5)).await;
    assert_eq!(row.status, "ok");
}

// ---------------------------------------------------------------------------
// A9f — the rebuild happens under the `lanes` lock, and one lane serves
// concurrent enqueues
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rebuild_holds_the_lanes_lock() {
    let fx = SeriesFixture::boot(started(Duration::from_secs(5))).await;
    let first = fx.write_series_block(seam_fixture()["block"].clone()).await;
    fx.reply_structured(seam_fixture()["reply"].clone());
    let resolver = fx.resolver().clone();

    // Kill the lane once.
    resolver.failpoints.panic_drain_once();
    assert_eq!(fx.enqueue(&first).await, Enqueue::Queued);
    let spawned_before = resolver.failpoints.drain_spawned();
    wait_until("the lane to die", Duration::from_secs(5), || {
        resolver.lane_is_finished(MARKET_PLUGIN_ID) == Some(true)
    })
    .await;

    // Task A enqueues into the dead lane and is held inside the rebuild,
    // blocking its worker thread while it holds the `lanes` lock.
    resolver.failpoints.hold_in_rebuild();
    let resolver_a = resolver.clone();
    let ctx_a = fx.ctx().clone();
    let track_a = fx.track_id().to_string();
    let block_a = first.clone();
    let request_a = fx.current_request(&first).await;
    let task_a = tokio::spawn(async move {
        resolver_a
            .enqueue(&ctx_a, &track_a, &block_a, &request_a)
            .await
    });
    wait_until("task A inside the rebuild", Duration::from_secs(5), || {
        resolver.failpoints.rebuild_entered() == 1
    })
    .await;
    // Sample the lock BEFORE releasing A: a held lock is the witness. The
    // release comes before the assertion so a red run does not leave A
    // blocked on a worker thread forever.
    let lock_was_free = resolver.lanes_try_lock();
    resolver.failpoints.release_rebuild();
    assert!(
        !lock_was_free,
        "the lanes lock must be held for the whole rebuild"
    );
    assert_eq!(task_a.await.expect("task A"), Enqueue::Queued);
    assert_eq!(
        resolver.failpoints.drain_spawned(),
        spawned_before + 1,
        "exactly one rebuild"
    );
    fx.wait_for_calls(1, Duration::from_secs(5)).await;

    // Eight concurrent enqueues on the same plugin, different blocks: still
    // that one lane, and the plugin sees eight calls.
    let mut blocks = Vec::new();
    for i in 0..8 {
        blocks.push(
            fx.write_series_block(json!({
                "source": SOURCE, "series": [format!("US:B{i}")], "as_of": "2026-09-10"
            }))
            .await,
        );
    }
    fx.reply_structured(json!({ "series": [] }));
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for block_id in blocks {
        let barrier = barrier.clone();
        let resolver = resolver.clone();
        let ctx = fx.ctx().clone();
        let track = fx.track_id().to_string();
        let request = fx.current_request(&block_id).await;
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            resolver.enqueue(&ctx, &track, &block_id, &request).await
        }));
    }
    for task in tasks {
        assert_eq!(task.await.expect("enqueue task"), Enqueue::Queued);
    }
    fx.wait_for_calls(9, Duration::from_secs(10)).await;
    assert_eq!(
        resolver.failpoints.drain_spawned(),
        spawned_before + 1,
        "a live lane is never rebuilt"
    );
    let calls = fx.calls();
    assert_eq!(calls.len(), 9);
    let mut seen: Vec<String> = calls[1..]
        .iter()
        .map(|c| c["arguments"]["series"][0].as_str().unwrap().to_string())
        .collect();
    seen.sort();
    let expected: Vec<String> = (0..8).map(|i| format!("US:B{i}")).collect();
    assert_eq!(seen, expected, "each block was called exactly once");
}
