//! The series resolver driven directly (`enqueue` + `resolve`).

#![cfg(unix)]

use std::time::Duration;

use calm_server::report_series::{Enqueue, ResolveOutcome, SERIES_TTL_MS, SeriesResolver};
use serde_json::{Value, json};

use crate::report_series_fixture::{
    DAY_MS, FORGE_TOOL_SOURCE, FixtureOptions, SOURCE, SeriesFixture, T0_MS, WRITE_TOOL_SOURCE,
    ok_series, seam_fixture, wait_until,
};

fn wrote(status: &str, pinned: bool) -> ResolveOutcome {
    ResolveOutcome::Wrote {
        status: status.into(),
        pinned,
    }
}

#[tokio::test]
async fn resolve_marks_a_hung_plugin_unavailable() {
    let fx = SeriesFixture::boot(FixtureOptions {
        resolve_timeout: Duration::from_millis(50),
        ..FixtureOptions::default()
    })
    .await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    fx.reply_hang();
    let (enqueued, outcomes) =
        tokio::time::timeout(Duration::from_secs(5), fx.resolve_block(&block_id))
            .await
            .expect("a hung plugin must not hang the resolver");
    assert_eq!(enqueued, Enqueue::Queued);
    assert_eq!(outcomes, vec![wrote("unavailable", false)]);
    let row = fx.row(&block_id).await.expect("row");
    assert_eq!(row.status, "unavailable");
    assert!(
        row.reason
            .as_deref()
            .is_some_and(|r| r.contains("timed out")),
        "{row:?}"
    );
    assert!(row.resolved_at >= T0_MS && row.resolved_at - T0_MS < 2 * 60 * 1000);
    assert!(!row.pinned);
    assert_eq!(fx.call_count(), 1, "the plugin did receive the call");
    assert_eq!(
        fx.resolver().inflight_len(),
        0,
        "key released after timeout"
    );
}

#[tokio::test]
async fn resolve_marks_error_and_malformed_replies_unavailable() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let errored = fx.write_series_block(seam_fixture()["block"].clone()).await;
    let mut other = seam_fixture()["block"].clone();
    other["range"] = json!("3M");
    let malformed = fx.write_series_block(other).await;

    fx.reply_is_error("source returned 503");
    assert_eq!(
        fx.resolve_block(&errored).await.1,
        vec![wrote("unavailable", false)]
    );
    fx.program(json!({ "mode": "raw", "result": {
        "content": [], "isError": false, "structuredContent": "not an object"
    }}));
    assert_eq!(
        fx.resolve_block(&malformed).await.1,
        vec![wrote("unavailable", false)]
    );

    let errored_row = fx.row(&errored).await.expect("row");
    let malformed_row = fx.row(&malformed).await.expect("row");
    assert_eq!(errored_row.status, "unavailable");
    assert_eq!(malformed_row.status, "unavailable");
    assert_eq!(
        errored_row.reason.as_deref(),
        Some("plugin error: source returned 503")
    );
    assert_eq!(
        malformed_row.reason.as_deref(),
        Some("reply structuredContent is not an object")
    );
    assert!(errored_row.summary.is_none() && errored_row.data.is_none());
    assert_eq!(fx.call_count(), 2);
}

#[tokio::test]
async fn frozen_row_is_pinned_by_first_complete_resolution_and_never_overwritten() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    let reply_a = json!({ "series": [
        ok_series("US:NVDA", "2026-09-11", &[("2026-08-11", 1.0), ("2026-09-10", 2.0)]),
        ok_series("HK:9988", "2026-09-11", &[("2026-08-11", 10.0), ("2026-09-10", 20.0)]),
    ]});
    let reply_b = json!({ "series": [
        ok_series("US:NVDA", "2026-09-12", &[("2026-08-11", 5.0), ("2026-09-10", 6.0)]),
        ok_series("HK:9988", "2026-09-12", &[("2026-08-11", 50.0), ("2026-09-10", 60.0)]),
    ]});
    let reply_c = json!({ "series": [
        ok_series("US:NVDA", "2026-09-14", &[("2026-08-11", 7.0), ("2026-09-10", 8.0)]),
        ok_series("HK:9988", "2026-09-14", &[("2026-08-11", 70.0), ("2026-09-10", 80.0)]),
    ]});
    // Two independent resolvers so two jobs for ONE key exist at once (the in-flight set would
    // otherwise coalesce them). Job 1 is parked at `hold_before_write`; job 2 runs to completion; job 1 is released last.
    let second = SeriesResolver::new_unstarted(fx.boot.repo.sqlite_pool())
        .with_now(std::sync::Arc::new(|| T0_MS));
    fx.program(json!({ "mode": "sequence", "replies": [
        { "mode": "structured", "structured": reply_a },
        { "mode": "structured", "structured": reply_b },
        { "mode": "structured", "structured": reply_c },
    ]}));
    let request = fx.current_request(&block_id).await;
    assert_eq!(fx.enqueue(&block_id).await, Enqueue::Queued);
    assert_eq!(
        second
            .enqueue(fx.ctx(), fx.track_id(), &block_id, &request)
            .await,
        Enqueue::Queued
    );
    let job_1 = fx.resolver().take_recorded_jobs().pop().unwrap();
    let job_2 = second.take_recorded_jobs().pop().unwrap();

    fx.resolver().failpoints.hold_before_write();
    let resolver_1 = fx.resolver().clone();
    let resolving_1 = tokio::spawn(async move { resolver_1.resolve(job_1).await });
    fx.wait_for_calls(1, Duration::from_secs(5)).await;
    let failpoints = &fx.resolver().failpoints;
    wait_until(
        "job 1 parked before its write",
        Duration::from_secs(5),
        || failpoints.write_held() == 1,
    )
    .await;
    assert!(
        fx.row(&block_id).await.is_none(),
        "nothing is written while job 1 is parked"
    );

    let out_2 = second.resolve(job_2).await;
    assert_eq!(out_2, wrote("ok", true), "job 2 finishes first and pins");
    assert_eq!(fx.call_count(), 2);
    let pinned = fx.row(&block_id).await.expect("row");
    assert!(pinned.pinned);
    assert_eq!(
        pinned.data_json()["series"][0]["points"][0][1],
        json!(5.0),
        "the row holds the FIRST finisher's data (job 2, reply B)"
    );

    fx.resolver().failpoints.release_write();
    let out_1 = resolving_1.await.expect("job 1 must not panic");
    assert_eq!(
        out_1,
        wrote("ok", true),
        "reply A satisfies the pin predicate too; the DB, not the outcome, refuses it"
    );
    assert_eq!(
        fx.row(&block_id).await.expect("row"),
        pinned,
        "job 1's later write did not overwrite the pinned row (byte-identical)"
    );

    let (enqueued, outcomes) = fx.resolve_block(&block_id).await;
    assert_eq!(enqueued, Enqueue::Queued);
    assert_eq!(
        outcomes,
        vec![ResolveOutcome::Dropped("row is fresh or pinned".into())]
    );
    assert_eq!(
        fx.row(&block_id).await.expect("row"),
        pinned,
        "byte-identical"
    );
    assert_eq!(fx.call_count(), 2, "no third call");
}

async fn frozen_not_pinned_then_pinned(as_of: &str) {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let block_id = fx
        .write_series_block(json!({
            "source": SOURCE, "series": ["US:NVDA"], "range": "1M", "as_of": as_of
        }))
        .await;
    // Friday's bar has no later bar proving it closed, so the reply stops at Thursday.
    fx.reply_structured(json!({ "series": [
        ok_series("US:NVDA", "2026-09-11", &[("2026-09-09", 1.0), ("2026-09-10", 2.0)])
    ]}));
    assert_eq!(
        fx.resolve_block(&block_id).await.1,
        vec![wrote("ok", false)]
    );
    let row = fx.row(&block_id).await.expect("row");
    assert_eq!(row.status, "ok");
    assert!(
        !row.pinned,
        "complete_through {} is not past as_of {as_of}",
        "2026-09-11"
    );
    assert_eq!(
        row.summary_json()["series"][0]["last"],
        json!(["2026-09-10", 2.0])
    );

    assert_eq!(
        fx.resolve_block(&block_id).await.1,
        vec![ResolveOutcome::Dropped("row is fresh or pinned".into())]
    );
    assert_eq!(fx.call_count(), 1);

    // TTL expired: Monday's bar is out, so Friday's bar is proven closed and included; the row pins.
    fx.advance_clock(SERIES_TTL_MS + 1);
    fx.reply_structured(json!({ "series": [
        ok_series("US:NVDA", "2026-09-14", &[
            ("2026-09-09", 1.0), ("2026-09-10", 2.0), ("2026-09-11", 3.0)
        ])
    ]}));
    assert_eq!(fx.resolve_block(&block_id).await.1, vec![wrote("ok", true)]);
    let row = fx.row(&block_id).await.expect("row");
    assert!(row.pinned);
    assert_eq!(
        row.summary_json()["series"][0]["last"],
        json!(["2026-09-11", 3.0])
    );
    assert_eq!(fx.call_count(), 2);
}

/// `as_of` = Friday, `complete_through` = Friday: equal is not "past".
#[tokio::test]
async fn frozen_reply_at_cutoff_is_not_pinned() {
    frozen_not_pinned_then_pinned("2026-09-11").await;
}

/// `as_of` = Sunday, `complete_through` = Friday: behind is not "past".
#[tokio::test]
async fn frozen_reply_behind_cutoff_is_not_pinned() {
    frozen_not_pinned_then_pinned("2026-09-13").await;
}

struct LiveRun {
    fx: SeriesFixture,
    block_id: String,
    doc_rev_before: u64,
    read_after_stale: Value,
    refresh_outcomes: Vec<ResolveOutcome>,
}

/// Day one at `T0` (Monday, yesterday = 09-13): resolve a live 1Y daily block. Day two (`T0 + 1d`):
/// the row is older than its TTL; a read serves it and enqueues; the plugin has one more bar and the job refreshes the row.
async fn live_two_days() -> LiveRun {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let block_id = fx
        .write_series_block(json!({ "source": SOURCE, "series": ["US:NVDA"] }))
        .await;
    fx.reply_structured(json!({ "series": [
        ok_series("US:NVDA", "2026-09-13", &[("2026-09-10", 1.0), ("2026-09-11", 2.0), ("2026-09-13", 3.0)])
    ]}));
    let first = fx.read(json!({})).await;
    let doc_rev_before = first["docRev"].as_u64().unwrap();
    assert_eq!(fx.run_recorded_jobs().await, vec![wrote("ok", false)]);
    let day_one = fx.row(&block_id).await.expect("row");
    assert_eq!(day_one.as_of, "2026-09-13");

    fx.advance_clock(DAY_MS);
    fx.reply_structured(json!({ "series": [
        ok_series("US:NVDA", "2026-09-14", &[("2026-09-11", 2.0), ("2026-09-13", 3.0), ("2026-09-14", 4.0)])
    ]}));
    let read_after_stale = fx.read(json!({})).await;
    let refresh_outcomes = fx.run_recorded_jobs().await;
    LiveRun {
        fx,
        block_id,
        doc_rev_before,
        read_after_stale,
        refresh_outcomes,
    }
}

#[tokio::test]
async fn stale_live_row_is_served_and_refreshed() {
    let run = live_two_days().await;
    let resolved = SeriesFixture::resolved_of(&run.read_after_stale, &run.block_id);
    assert_eq!(
        resolved["status"], "ok",
        "the stale row is served: {resolved}"
    );
    assert_eq!(resolved["as_of"], "2026-09-13", "…as it was");
    assert_eq!(
        run.fx.resolver().recorded_outcomes(),
        vec![Enqueue::Queued, Enqueue::Queued],
        "the read of a stale row enqueues"
    );
    assert_eq!(run.refresh_outcomes, vec![wrote("ok", false)]);
    assert_eq!(run.fx.call_count(), 2);
    let row = run.fx.row(&run.block_id).await.expect("row");
    assert_eq!(
        row.summary_json()["series"][0]["last"],
        json!(["2026-09-14", 4.0])
    );
    assert_eq!(
        run.read_after_stale["docRev"].as_u64().unwrap(),
        run.doc_rev_before,
        "a refresh is not a document edit"
    );
    assert!(!row.pinned, "live rows never pin");
}

#[tokio::test]
async fn live_request_carries_yesterday_utc_cutoff() {
    let run = live_two_days().await;
    let calls = run.fx.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["arguments"]["mode"], "live");
    assert_eq!(calls[0]["arguments"]["as_of"], "2026-09-13");
    assert_eq!(calls[0]["arguments"]["start"], "2025-09-12", "as_of - 366d");
    assert_eq!(calls[1]["arguments"]["as_of"], "2026-09-14");
    assert_eq!(calls[1]["arguments"]["start"], "2025-09-13");
}

#[tokio::test]
async fn refreshed_live_row_advances_as_of() {
    let run = live_two_days().await;
    assert_eq!(run.refresh_outcomes, vec![wrote("ok", false)]);
    let row = run.fx.row(&run.block_id).await.expect("row");
    assert_eq!(
        row.as_of, "2026-09-14",
        "the row's cutoff moved with the clock"
    );
    assert_eq!(run.fx.rows().await.len(), 1, "same hash, same row");
}

#[tokio::test]
async fn live_daily_row_includes_yesterday() {
    let run = live_two_days().await;
    assert_eq!(
        run.refresh_outcomes,
        vec![wrote("ok", false)],
        "a bar dated complete_through is accepted for live daily"
    );
    let row = run.fx.row(&run.block_id).await.expect("row");
    assert_eq!(row.status, "ok", "{row:?}");
    assert_eq!(row.summary_json()["series"][0]["n"], 3);
    assert_eq!(row.summary_json()["series"][0]["last"][0], "2026-09-14");
}

#[tokio::test]
async fn fresh_row_is_not_re_resolved_by_late_reader() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    // A frozen reply that does NOT pin (complete_through == as_of), so only the TTL stands between a late job and a second call.
    fx.reply_structured(json!({ "series": [
        ok_series("US:NVDA", "2026-09-10", &[("2026-08-11", 1.0), ("2026-09-09", 2.0)]),
        ok_series("HK:9988", "2026-09-10", &[("2026-08-11", 1.0), ("2026-09-09", 2.0)]),
    ]}));
    assert_eq!(
        fx.resolve_block(&block_id).await.1,
        vec![wrote("ok", false)]
    );
    assert_eq!(fx.call_count(), 1);
    let (enqueued, outcomes) = fx.resolve_block(&block_id).await;
    assert_eq!(enqueued, Enqueue::Queued, "the key is free, so it queues");
    assert_eq!(
        outcomes,
        vec![ResolveOutcome::Dropped("row is fresh or pinned".into())]
    );
    assert_eq!(fx.call_count(), 1, "admission dropped the late job");
}

#[tokio::test]
async fn unavailable_row_is_retried_after_two_minutes() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    fx.reply_is_error("boom");
    assert_eq!(
        fx.resolve_block(&block_id).await.1,
        vec![wrote("unavailable", false)]
    );
    assert_eq!(fx.call_count(), 1);

    fx.set_clock(T0_MS + 60 * 1000);
    assert_eq!(
        fx.resolve_block(&block_id).await.1,
        vec![ResolveOutcome::Dropped("row is fresh or pinned".into())],
        "one minute old: still fresh"
    );
    assert_eq!(fx.call_count(), 1);

    fx.set_clock(T0_MS + 3 * 60 * 1000);
    assert_eq!(
        fx.resolve_block(&block_id).await.1,
        vec![wrote("unavailable", false)],
        "three minutes old: retried"
    );
    assert_eq!(fx.call_count(), 2);
}

#[tokio::test]
async fn partial_ok_row_is_retried_after_two_minutes() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let partial = fx.write_series_block(seam_fixture()["block"].clone()).await;
    let mut other = seam_fixture()["block"].clone();
    other["range"] = json!("3M");
    let all_ok = fx.write_series_block(other).await;

    fx.reply_structured(json!({ "series": [
        ok_series("US:NVDA", "2026-09-11", &[("2026-08-11", 1.0), ("2026-09-10", 2.0)]),
        { "asset": "HK:9988", "status": "unavailable", "reason": "source 503" },
    ]}));
    assert_eq!(fx.resolve_block(&partial).await.1, vec![wrote("ok", false)]);
    fx.reply_structured(json!({ "series": [
        ok_series("US:NVDA", "2026-09-10", &[("2026-08-11", 1.0), ("2026-09-09", 2.0)]),
        ok_series("HK:9988", "2026-09-10", &[("2026-08-11", 1.0), ("2026-09-09", 2.0)]),
    ]}));
    assert_eq!(fx.resolve_block(&all_ok).await.1, vec![wrote("ok", false)]);
    assert_eq!(fx.call_count(), 2);

    fx.set_clock(T0_MS + 3 * 60 * 1000);
    // The second reply DIFFERS in the series that already succeeded, so a merge with the old row would not pass.
    fx.reply_structured(json!({ "series": [
        ok_series("US:NVDA", "2026-09-11", &[("2026-08-11", 100.0), ("2026-09-10", 200.0)]),
        ok_series("HK:9988", "2026-09-11", &[("2026-08-11", 10.0), ("2026-09-10", 20.0)]),
    ]}));
    assert_eq!(
        fx.resolve_block(&partial).await.1,
        vec![wrote("ok", true)],
        "three minutes old with a non-ok series: retried"
    );
    assert_eq!(fx.call_count(), 3);
    let row = fx.row(&partial).await.expect("row");
    let summary = row.summary_json();
    assert_eq!(summary["series"][0]["first"], json!(["2026-08-11", 100.0]));
    assert_eq!(summary["series"][0]["last"], json!(["2026-09-10", 200.0]));
    assert_eq!(summary["series"][1]["status"], "ok");
    assert_eq!(
        row.data_json()["series"][0]["points"][0][1],
        json!(100.0),
        "new row = second reply, whole"
    );

    assert_eq!(
        fx.resolve_block(&all_ok).await.1,
        vec![ResolveOutcome::Dropped("row is fresh or pinned".into())],
        "three minutes old but every series ok: 6h TTL, still fresh"
    );
    assert_eq!(fx.call_count(), 3);
}

#[tokio::test]
async fn failed_resolve_releases_inflight_key() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    fx.reply_structured(seam_fixture()["reply"].clone());
    fx.resolver().failpoints.fail_write_once();
    let (_, outcomes) = fx.resolve_block(&block_id).await;
    assert!(
        matches!(outcomes.as_slice(), [ResolveOutcome::WriteFailed(_)]),
        "{outcomes:?}"
    );
    assert_eq!(fx.call_count(), 1);
    assert!(fx.rows().await.is_empty());
    assert_eq!(
        fx.resolver().inflight_len(),
        0,
        "the guard released the key"
    );

    let (enqueued, outcomes) = fx.resolve_block(&block_id).await;
    assert_eq!(enqueued, Enqueue::Queued, "not fail-locked");
    assert_eq!(outcomes, vec![wrote("ok", true)]);
    assert_eq!(fx.call_count(), 2);
}

#[tokio::test]
async fn resolve_refuses_non_read_only_tools() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    fx.reply_structured(seam_fixture()["reply"].clone());
    let write_tool = fx
        .write_series_block(json!({ "source": WRITE_TOOL_SOURCE, "series": ["US:NVDA"] }))
        .await;
    let forge_tool = fx
        .write_series_block(json!({ "source": FORGE_TOOL_SOURCE, "series": ["US:NVDA"] }))
        .await;
    for block_id in [&write_tool, &forge_tool] {
        let (enqueued, outcomes) = fx.resolve_block(block_id).await;
        assert_eq!(enqueued, Enqueue::Queued, "exposed and running: it routes");
        assert_eq!(outcomes, vec![wrote("unavailable", false)]);
        let row = fx.row(block_id).await.expect("row");
        assert_eq!(
            row.reason.as_deref(),
            Some("tool is not an ordinary read-only tool")
        );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fx.call_count(), 0, "refused before any call");
}

#[tokio::test]
async fn late_resolution_after_track_delete_leaves_no_orphan() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    fx.reply_structured(seam_fixture()["reply"].clone());
    assert_eq!(fx.enqueue(&block_id).await, Enqueue::Queued);
    let job = fx.resolver().take_recorded_jobs().pop().unwrap();
    fx.resolver().failpoints.hold_before_write();
    let resolver = fx.resolver().clone();
    let resolving = tokio::spawn(async move { resolver.resolve(job).await });
    // The job is parked at the write when the track goes away, so the delete cannot lose a race with the write.
    let failpoints = &fx.resolver().failpoints;
    wait_until(
        "the job parked before its write",
        Duration::from_secs(5),
        || failpoints.write_held() == 1,
    )
    .await;
    assert_eq!(fx.call_count(), 1);
    fx.boot
        .repo
        .track_delete(fx.track_id())
        .await
        .expect("delete track");
    fx.resolver().failpoints.release_write();
    let outcome = resolving.await.expect("the task must not panic");
    assert!(
        matches!(outcome, ResolveOutcome::WriteFailed(_)),
        "the FK refuses the late write: {outcome:?}"
    );
    let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM report_series")
        .fetch_one(&fx.boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(orphans, 0, "no orphan row");
    assert_eq!(fx.resolver().inflight_len(), 0, "key released");
}

#[tokio::test]
async fn reason_is_capped() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let block_id = fx.write_series_block(seam_fixture()["block"].clone()).await;
    let long: String = "é".repeat(10_000);
    fx.reply_is_error(&long);
    assert_eq!(
        fx.resolve_block(&block_id).await.1,
        vec![wrote("unavailable", false)]
    );
    let row = fx.row(&block_id).await.expect("row");
    let reason = row.reason.expect("reason");
    assert_eq!(reason.chars().count(), 256);
    assert!(reason.starts_with("plugin error: é"));
}
