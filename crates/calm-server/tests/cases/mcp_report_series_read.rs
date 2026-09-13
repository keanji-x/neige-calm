//! #1628 S2 — `calm.report.read` hydration of `chart.series` blocks through
//! the real tool (design §6 A4, A5 read-side, A6, A9e, A9h, A10, A10b, A11).
//!
//! The resolver is unstarted here: a read records what it would enqueue and
//! the test runs the job by hand, so "the read did not call the plugin" is
//! a count of calls the fake plugin logged, not a timing argument.

#![cfg(unix)]

use std::time::Duration;

use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_COMMIT;
use calm_server::report_series::{Enqueue, ResolveOutcome};
use calm_types::report_blocks::KIND_CHART_SERIES;
use serde_json::{Value, json};

use crate::mcp_track_report::{call_tool, planner_identity};
use crate::report_series_fixture::{
    FixtureOptions, MARKET_PLUGIN_ID, SOURCE, SeriesFixture, UNDERSCORE_PLUGIN_ID,
    UNDERSCORE_SOURCE, UNEXPOSED_SOURCE, UNINSTALLED_SOURCE, ok_series, seam_fixture,
};

fn frozen_block() -> Value {
    seam_fixture()["block"].clone()
}

// ---------------------------------------------------------------------------
// A4 — the default read hands out the stored summary
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_hydrates_chart_series_summary_from_row() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let seam = seam_fixture();
    let block_id = fx.write_series_block(seam["block"].clone()).await;

    // No row yet: pending, and the read enqueued exactly one job.
    let first = fx.read(json!({})).await;
    let resolved = SeriesFixture::resolved_of(&first, &block_id);
    assert_eq!(resolved["status"], "pending", "{resolved}");
    assert_eq!(fx.resolver().recorded_outcomes(), vec![Enqueue::Queued]);

    fx.reply_structured(seam["reply"].clone());
    let outcomes = fx.run_recorded_jobs().await;
    assert_eq!(
        outcomes,
        vec![ResolveOutcome::Wrote {
            status: "ok".into(),
            pinned: true
        }]
    );
    // The request the plugin saw is the seam's request (minus deadline).
    let calls = fx.calls();
    assert_eq!(calls.len(), 1);
    let mut args = calls[0]["arguments"].clone();
    assert!(args["deadline_ms"].is_i64(), "{args}");
    args.as_object_mut().unwrap().remove("deadline_ms");
    assert_eq!(args, seam["request"]);
    assert_eq!(
        calls[0]["_meta"]["dev.neige/track"]["id"],
        json!(fx.track_id()),
        "the kernel names the track in _meta"
    );

    let second = fx.read(json!({})).await;
    let mut resolved = SeriesFixture::resolved_of(&second, &block_id).clone();
    let resolved_at = resolved["resolved_at"]
        .as_str()
        .expect("resolved_at")
        .to_string();
    assert!(resolved_at.ends_with('Z'), "RFC 3339 UTC: {resolved_at}");
    resolved.as_object_mut().unwrap().remove("resolved_at");
    assert_eq!(resolved, seam["resolved"], "summary comes from the row");
    // Summary by default: no points anywhere.
    for entry in resolved["series"].as_array().unwrap() {
        assert!(entry.get("points").is_none(), "{entry}");
    }
    // A second read of a fresh pinned row enqueues nothing more.
    assert_eq!(fx.resolver().recorded_outcomes().len(), 1);
}

// ---------------------------------------------------------------------------
// A5 (read side) — route misses are `pending` with a reason, no row, no job
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_reports_route_misses_as_pending_without_rows() {
    let fx = SeriesFixture::boot(FixtureOptions {
        spawn_market: false,
        ..FixtureOptions::default()
    })
    .await;
    let not_installed = fx
        .write_series_block(json!({ "source": UNINSTALLED_SOURCE, "series": ["US:NVDA"] }))
        .await;
    let not_exposed = fx
        .write_series_block(json!({ "source": UNEXPOSED_SOURCE, "series": ["US:NVDA"] }))
        .await;
    let not_running = fx
        .write_series_block(json!({ "source": SOURCE, "series": ["US:NVDA"] }))
        .await;

    let read = fx.read(json!({})).await;
    let expectations = [
        (&not_installed, "plugin nobody is not installed"),
        (
            &not_exposed,
            "plugin dev-neige-market does not expose market.nothing",
        ),
        (&not_running, "plugin dev-neige-market is not running"),
    ];
    for (block_id, reason) in expectations {
        let resolved = SeriesFixture::resolved_of(&read, block_id);
        assert_eq!(resolved["status"], "pending", "{resolved}");
        assert_eq!(resolved["reason"], reason, "{resolved}");
    }
    assert!(fx.rows().await.is_empty(), "misses never store a row");
    assert!(
        fx.resolver()
            .recorded_outcomes()
            .iter()
            .all(|o| matches!(o, Enqueue::Miss(_))),
        "{:?}",
        fx.resolver().recorded_outcomes()
    );
    assert!(fx.resolver().take_recorded_jobs().is_empty(), "no job");
    assert_eq!(fx.resolver().inflight_len(), 0, "keys released on miss");
    assert!(read["docRev"].is_u64(), "the read itself is whole: {read}");
}

// ---------------------------------------------------------------------------
// A6 — `resolve: { id: "full" }` is the only way to get points
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_full_is_opt_in_per_block() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let seam = seam_fixture();
    let a = fx.write_series_block(seam["block"].clone()).await;
    let mut other = seam["block"].clone();
    other["series"] = json!(["US:NVDA", "HK:9988", "US:AAPL"]);
    let b = fx.write_series_block(other).await;
    let mut reply_b = seam["reply"].clone();
    reply_b["series"].as_array_mut().unwrap().push(ok_series(
        "US:AAPL",
        "2026-09-11",
        &[("2026-08-11", 1.0), ("2026-09-10", 2.0)],
    ));
    fx.read(json!({})).await;
    // Jobs run in block order: a gets the seam reply, b its three-series one.
    fx.program(json!({ "mode": "sequence", "replies": [
        { "mode": "structured", "structured": seam["reply"] },
        { "mode": "structured", "structured": reply_b },
    ]}));
    let outcomes = fx.run_recorded_jobs().await;
    assert_eq!(
        outcomes,
        vec![
            ResolveOutcome::Wrote {
                status: "ok".into(),
                pinned: true
            },
            ResolveOutcome::Wrote {
                status: "ok".into(),
                pinned: true
            },
        ]
    );

    let default_read = fx.read(json!({})).await;
    for block_id in [&a, &b] {
        let resolved = SeriesFixture::resolved_of(&default_read, block_id);
        for entry in resolved["series"].as_array().unwrap_or(&vec![]) {
            assert!(entry.get("points").is_none(), "default read: {entry}");
        }
    }

    let full_a = fx.read(json!({ "resolve": { a.as_str(): "full" } })).await;
    let resolved_a = SeriesFixture::resolved_of(&full_a, &a);
    assert_eq!(resolved_a["status"], "ok", "{resolved_a}");
    assert_eq!(
        resolved_a["series"][0]["points"], seam["reply"]["series"][0]["points"],
        "full: points come from the same row's data column"
    );
    assert_eq!(
        resolved_a["series"][1]["points"],
        seam["reply"]["series"][1]["points"]
    );
    let resolved_b = SeriesFixture::resolved_of(&full_a, &b);
    for entry in resolved_b["series"].as_array().unwrap_or(&vec![]) {
        assert!(
            entry.get("points").is_none(),
            "unnamed block stays summary: {entry}"
        );
    }

    let none = fx.read(json!({ "resolve": { a.as_str(): "none" } })).await;
    let entry = none["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|blk| blk["id"] == json!(a))
        .unwrap();
    assert!(
        entry.get("resolved").is_none(),
        "none skips the block: {entry}"
    );

    let err = call_tool(
        &fx.boot,
        "calm.report.read",
        planner_identity(&fx.boot),
        json!({ "resolve": { a.as_str(): "summary" } }),
    )
    .await
    .expect_err("summary is the default, not a value");
    assert_eq!(err.code, -32602, "{err:?}");
}

// ---------------------------------------------------------------------------
// A9e — a pre-check miss freezes nothing: the next read after the plugin
// starts queues the job
// ---------------------------------------------------------------------------

#[tokio::test]
async fn precheck_miss_never_lands_a_row() {
    let fx = SeriesFixture::boot(FixtureOptions {
        spawn_market: false,
        ..FixtureOptions::default()
    })
    .await;
    let seam = seam_fixture();
    let block_id = fx.write_series_block(seam["block"].clone()).await;

    let read = fx.read(json!({})).await;
    let resolved = SeriesFixture::resolved_of(&read, &block_id);
    assert_eq!(resolved["status"], "pending");
    assert!(
        resolved["reason"]
            .as_str()
            .is_some_and(|r| r.contains("is not running")),
        "{resolved}"
    );
    assert!(fx.rows().await.is_empty());
    assert_eq!(
        fx.resolver().recorded_outcomes(),
        vec![Enqueue::Miss(format!(
            "plugin {MARKET_PLUGIN_ID} is not running"
        ))]
    );
    assert!(fx.resolver().take_recorded_jobs().is_empty());

    fx.plugin_host
        .spawn(MARKET_PLUGIN_ID)
        .await
        .expect("spawn market");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if fx
            .plugin_host
            .running_plugin_ids()
            .await
            .contains(MARKET_PLUGIN_ID)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "market did not start"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let read = fx.read(json!({})).await;
    assert_eq!(
        SeriesFixture::resolved_of(&read, &block_id)["status"],
        "pending"
    );
    assert_eq!(
        fx.resolver().recorded_outcomes().last(),
        Some(&Enqueue::Queued),
        "the read after the plugin started queues"
    );
    fx.reply_structured(seam["reply"].clone());
    let outcomes = fx.run_recorded_jobs().await;
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(fx.call_count(), 1);
    let row = fx.row(&block_id).await.expect("row landed");
    assert_eq!(row.status, "ok");
}

// ---------------------------------------------------------------------------
// A9h — admission and the read end select the row for the CURRENT hash
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admission_selects_current_hash_row() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let seam = seam_fixture();
    let block_id = fx.write_series_block(seam["block"].clone()).await;
    let h1 = fx.current_request(&block_id).await.request_hash;

    fx.read(json!({})).await;
    fx.reply_structured(seam["reply"].clone());
    fx.run_recorded_jobs().await;
    let h1_row = fx.row(&block_id).await.expect("h1 row");
    assert!(h1_row.pinned, "h1 is pinned");
    assert_eq!(fx.call_count(), 1);

    // Same block, new series → new hash; the pinned h1 row stays.
    let mut rewritten = seam["block"].clone();
    rewritten["series"] = json!(["US:NVDA"]);
    fx.rewrite_series_block(&block_id, rewritten).await;
    let h2 = fx.current_request(&block_id).await.request_hash;
    assert_ne!(h1, h2);

    let read = fx.read(json!({})).await;
    assert_eq!(
        SeriesFixture::resolved_of(&read, &block_id)["status"],
        "pending",
        "h1's pinned row must not answer for h2"
    );
    assert_eq!(
        fx.resolver().recorded_outcomes().last(),
        Some(&Enqueue::Queued)
    );
    fx.reply_structured(json!({
        "series": [ok_series("US:NVDA", "2026-09-11", &[("2026-08-11", 1.0), ("2026-09-10", 3.0)])]
    }));
    let outcomes = fx.run_recorded_jobs().await;
    assert_eq!(
        outcomes,
        vec![ResolveOutcome::Wrote {
            status: "ok".into(),
            pinned: true
        }],
        "admission must not be stopped by the h1 row"
    );
    let calls = fx.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1]["arguments"]["series"], json!(["US:NVDA"]));

    let rows = fx.rows().await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    let h1_after = rows.iter().find(|r| r.request_hash == h1).expect("h1 kept");
    assert_eq!(h1_after, &h1_row, "h1 untouched");
    let h2_row = rows
        .iter()
        .find(|r| r.request_hash == h2)
        .expect("h2 landed");
    assert_eq!(h2_row.status, "ok");

    let read = fx.read(json!({})).await;
    let resolved = SeriesFixture::resolved_of(&read, &block_id);
    assert_eq!(
        resolved["series"].as_array().unwrap().len(),
        1,
        "{resolved}"
    );
    assert_eq!(resolved["series"][0]["n"], 2, "the read returns h2's row");
    assert_eq!(resolved["series"][0]["last"], json!(["2026-09-10", 3.0]));
}

// ---------------------------------------------------------------------------
// A10 — the read path never calls the plugin
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_never_calls_the_plugin() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    let block_id = fx.write_series_block(frozen_block()).await;
    fx.reply_structured(seam_fixture()["reply"].clone());
    for _ in 0..3 {
        let read = fx.read(json!({})).await;
        assert_eq!(
            SeriesFixture::resolved_of(&read, &block_id)["status"],
            "pending"
        );
    }
    // Give a wrongly-inlined call every chance to land before counting.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(fx.call_count(), 0, "reads only read rows");
    assert!(fx.rows().await.is_empty(), "reads never write");
    assert_eq!(
        fx.resolver().recorded_outcomes(),
        vec![Enqueue::Queued, Enqueue::InFlight, Enqueue::InFlight]
    );
}

// ---------------------------------------------------------------------------
// A10b — the write path does not trigger resolution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_does_not_trigger_resolution() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    fx.reply_structured(seam_fixture()["reply"].clone());
    // `commit` without any read: docRev of a fresh report is known (S1 pins
    // the initial document at 3 after boot's seeding).
    let if_doc_rev = {
        let card = fx
            .boot
            .repo
            .card_get(fx.boot.report_card_id.as_str())
            .await
            .unwrap()
            .expect("report card");
        let payload: calm_server::track_report::TrackReportPayload =
            serde_json::from_value(card.payload).unwrap();
        payload.doc_rev
    };
    call_tool(
        &fx.boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&fx.boot),
        json!({
            "if_doc_rev": if_doc_rev,
            "message": "one chart.series block, never read",
            "ops": [{ "op": "upsert", "kind": KIND_CHART_SERIES, "payload": frozen_block() }]
        }),
    )
    .await
    .expect("commit succeeds");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        fx.resolver().recorded_enqueue_calls(),
        0,
        "no enqueue from a write"
    );
    assert!(fx.rows().await.is_empty());
    assert_eq!(fx.call_count(), 0);
}

// ---------------------------------------------------------------------------
// A11 — `aa_b` is looked up exactly, never re-parsed into `aa` + `b_c`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn underscore_plugin_id_never_routes() {
    let fx = SeriesFixture::boot(FixtureOptions::default()).await;
    assert!(
        fx.plugin_host
            .running_plugin_ids()
            .await
            .contains(UNDERSCORE_PLUGIN_ID),
        "the probe plugin `aa` is running and exposes `b_c`"
    );
    let block_id = fx
        .write_series_block(json!({ "source": UNDERSCORE_SOURCE, "series": ["US:NVDA"] }))
        .await;
    let read = fx.read(json!({})).await;
    let resolved = SeriesFixture::resolved_of(&read, &block_id);
    assert_eq!(resolved["status"], "pending", "{resolved}");
    assert_eq!(
        resolved["reason"], "plugin aa_b is not installed",
        "the reason carries the source's own plugin id"
    );
    assert!(fx.rows().await.is_empty());
    assert!(fx.resolver().take_recorded_jobs().is_empty());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        fx.underscore_call_count(),
        0,
        "plugin `aa` was never called"
    );
}
