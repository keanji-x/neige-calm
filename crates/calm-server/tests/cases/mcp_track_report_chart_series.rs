//! `chart.series` through the real MCP write ends (`calm.report.blocks.upsert` and a `calm.report.commit` upsert op).
//! A cutoff in the future is accepted on purpose: `calm-types` has no clock.

#![cfg(unix)]

use std::time::Duration;

use crate::mcp_track_report::{Boot, boot, call_tool, planner_identity};
use calm_server::mcp_server::tools::track_report_blocks::{
    TOOL_REPORT_BLOCKS_UPSERT, TOOL_REPORT_COMMIT,
};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::track_report::TrackReportPayload;
use calm_types::report_blocks::{KIND_CHART_SERIES, parse_fence, split_body};
use serde_json::{Value, json};

const TOOL_REPORT_READ: &str = "calm.report.read";
const SOURCE: &str = "neige://plugin/dev-neige-market/market.series";

async fn read(boot: &Boot, args: Value) -> Value {
    call_tool(boot, TOOL_REPORT_READ, planner_identity(boot), args)
        .await
        .expect("planner can read the report")
}

async fn doc_rev(boot: &Boot) -> u64 {
    read(boot, json!({})).await["docRev"]
        .as_u64()
        .expect("read returns docRev")
}

async fn current_payload(boot: &Boot) -> TrackReportPayload {
    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .expect("report card row");
    serde_json::from_value(card.payload).expect("payload deserializes")
}

/// Drain everything the bus delivers within a short quiet window so a test can assert an exact event count.
async fn drain_events(
    rx: &mut tokio::sync::broadcast::Receiver<calm_server::event::BroadcastEnvelope>,
) -> Vec<calm_server::event::BroadcastEnvelope> {
    let mut out = Vec::new();
    while let Ok(Ok(env)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
        out.push(env);
    }
    out
}

/// Every `chart.series` fence in the flat body the planner reads back, parsed with the kernel's own read path.
fn chart_series_fences(read_out: &Value) -> Vec<Value> {
    let text = read_out["text"].as_str().expect("read returns text");
    split_body(text)
        .iter()
        .filter_map(|slice| parse_fence(&slice.raw))
        .filter(|fence| fence.kind == KIND_CHART_SERIES)
        .map(|fence| fence.payload)
        .collect()
}

/// One `calm.report.commit` carrying exactly one `chart.series` upsert op.
async fn commit_one_series(boot: &Boot, payload: Value) -> Result<Value, RpcError> {
    let if_doc_rev = doc_rev(boot).await;
    call_tool(
        boot,
        TOOL_REPORT_COMMIT,
        planner_identity(boot),
        json!({
            "if_doc_rev": if_doc_rev,
            "message": "one chart.series block",
            "ops": [
                { "op": "upsert", "kind": KIND_CHART_SERIES, "payload": payload }
            ]
        }),
    )
    .await
}

/// The refusal contract shared by the three rejection cases: `-32602` naming the field, the doc untouched, zero events.
async fn assert_commit_refused(boot: &Boot, payload: Value, needle: &str) {
    let before = current_payload(boot).await;
    let mut rx = boot.ctx.events.subscribe();
    let err = commit_one_series(boot, payload.clone())
        .await
        .expect_err("payload must be refused at the write end");
    assert_eq!(err.code, RpcError::INVALID_PARAMS, "{payload} → {err:?}");
    assert_eq!(err.code, -32602);
    assert!(err.message.contains(needle), "{payload} → {err:?}");
    let after = current_payload(boot).await;
    assert_eq!(after.doc_rev, before.doc_rev, "docRev untouched");
    assert_eq!(after.body, before.body, "body untouched");
    assert!(
        chart_series_fences(&read(boot, json!({})).await).is_empty(),
        "nothing landed"
    );
    assert!(drain_events(&mut rx).await.is_empty(), "nothing emitted");
}

#[tokio::test]
async fn upsert_chart_series_lands_as_canonical_fence() {
    let boot = boot().await;
    let payload = json!({
        "source": SOURCE,
        "series": ["US:NVDA", "HK:9988"],
        "as_of": "2026-09-10"
    });
    let if_doc_rev = doc_rev(&boot).await;
    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        planner_identity(&boot),
        json!({ "kind": KIND_CHART_SERIES, "payload": payload, "if_doc_rev": if_doc_rev }),
    )
    .await
    .expect("chart.series upsert succeeds");
    let id = out["id"].as_str().expect("upsert returns id").to_string();
    assert_eq!(out["rev"].as_u64(), Some(1));
    assert_eq!(out["docRev"].as_u64(), Some(if_doc_rev + 1));

    let read_out = read(&boot, json!({})).await;
    let text = read_out["text"].as_str().unwrap();
    assert!(
        text.contains("```neige-block chart.series\n"),
        "flat body carries the fence opener: {text}"
    );
    assert_eq!(chart_series_fences(&read_out), vec![payload.clone()]);
    let index = read_out["blocks"].as_array().expect("blocks index");
    let entry = index
        .iter()
        .find(|b| b["id"] == json!(id))
        .expect("the new block is in the index");
    assert_eq!(entry["kind"], json!(KIND_CHART_SERIES));

    let stored = current_payload(&boot).await;
    let block = stored
        .blocks
        .expect("blocks cache")
        .into_iter()
        .find(|b| b.kind == KIND_CHART_SERIES)
        .expect("chart.series block is cached");
    assert_eq!(block.id, id);
    assert_eq!(block.payload, payload);
}

#[tokio::test]
async fn commit_rejects_chart_series_without_venue() {
    let boot = boot().await;
    assert_commit_refused(
        &boot,
        json!({ "source": SOURCE, "series": ["NVDA"] }),
        "series[0]: must be a venue-qualified asset id",
    )
    .await;
}

#[tokio::test]
async fn commit_rejects_non_calendar_as_of() {
    let boot = boot().await;
    // Right shape, no such day: February has no 30th.
    assert_commit_refused(
        &boot,
        json!({ "source": SOURCE, "series": ["US:NVDA"], "as_of": "2026-02-30" }),
        "as_of: must be a calendar date in YYYY-MM-DD form, got `2026-02-30`",
    )
    .await;
    assert_commit_refused(
        &boot,
        json!({ "source": SOURCE, "series": ["US:NVDA"], "as_of": "2026/09/10" }),
        "as_of: must be a calendar date in YYYY-MM-DD form, got `2026/09/10`",
    )
    .await;
}

#[tokio::test]
async fn commit_rejects_month_period_in_one_month_range() {
    let boot = boot().await;
    assert_commit_refused(
        &boot,
        json!({ "source": SOURCE, "series": ["US:NVDA"], "range": "1M", "period": "month" }),
        "range 1M cannot hold two complete month periods",
    )
    .await;
}

#[tokio::test]
async fn commit_accepts_future_as_of() {
    let boot = boot().await;
    let payload = json!({ "source": SOURCE, "series": ["US:NVDA"], "as_of": "2099-01-01" });
    let before = doc_rev(&boot).await;
    let out = commit_one_series(&boot, payload.clone())
        .await
        .expect("a cutoff in the future is not a write-end error");
    assert_eq!(out["docRev"].as_u64(), Some(before + 1));

    let read_out = read(&boot, json!({})).await;
    assert_eq!(chart_series_fences(&read_out), vec![payload]);
    let kinds: Vec<&str> = read_out["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["kind"].as_str())
        .filter(|kind| *kind == KIND_CHART_SERIES)
        .collect();
    assert_eq!(
        kinds,
        [KIND_CHART_SERIES],
        "exactly one chart.series landed"
    );
}
