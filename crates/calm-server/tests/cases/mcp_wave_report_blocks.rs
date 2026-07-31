//! Issue #960 PR2 — typed `calm.report.blocks.*` + `write_markdown`
//! integration coverage.
//!
//! Reuses the `mcp_wave_report` fixture (in-memory `SqlxRepo`,
//! pre-seeded role cache, direct handler invocation). Coverage:
//!
//!   1. `blocks.kinds` returns the prose schema.
//!   2. `read` returns the `blocks` index + `text`/`body` alias;
//!      `with_markers` injects marker lines, default output is clean.
//!   3. `blocks.upsert` create / insert-at-position / replace-with-
//!      `if_rev`; missing `if_rev` on replace and non-prose kinds are
//!      invalid-params; a rev conflict returns `-32001`, writes
//!      nothing and emits nothing.
//!   4. `blocks.move` reorders (rev untouched) and honors `if_rev`.
//!   5. `blocks.delete` requires `if_rev` and honors it.
//!   6. Dual-event invariant: one successful block op → exactly one
//!      `CardUpdated` + one `WaveReportEdited` with flat-projection
//!      bodies and an unchanged summary.
//!   7. `write_markdown`: marker lines pin block ids and are stripped
//!      from storage + events (hard assertion), markerless bodies fall
//!      back to LCS alignment.

#![cfg(unix)]

use std::time::Duration;

use crate::mcp_wave_report::{Boot, boot, call_tool, collect_n, spec_identity, worker_identity};
use calm_server::event::Event;
use calm_server::mcp_server::tools::wave_report::{TOOL_REPORT_EDIT, TOOL_REPORT_WRITE};
use calm_server::mcp_server::tools::wave_report_blocks::{
    RPC_REV_CONFLICT, TOOL_REPORT_BLOCKS_DELETE, TOOL_REPORT_BLOCKS_KINDS, TOOL_REPORT_BLOCKS_MOVE,
    TOOL_REPORT_BLOCKS_UPSERT, TOOL_REPORT_WRITE_MARKDOWN,
};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::wave_report::WaveReportPayload;
use serde_json::{Value, json};

const TOOL_REPORT_READ: &str = "calm.report.read";
const SEED_BODY: &str = "# 概要\n\n_Spec agent 会在第一次 turn 时填这里。_\n";

/// Current report payload straight from the card row.
async fn current_payload(boot: &Boot) -> WaveReportPayload {
    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .expect("report card row");
    serde_json::from_value(card.payload).expect("payload deserializes")
}

/// `[(id, rev)]` from a `calm.report.read` response's blocks index.
fn index_of(read: &Value) -> Vec<(String, u64)> {
    read.get("blocks")
        .and_then(Value::as_array)
        .expect("read returns blocks array")
        .iter()
        .map(|b| {
            (
                b.get("id").and_then(Value::as_str).unwrap().to_string(),
                b.get("rev").and_then(Value::as_u64).unwrap(),
            )
        })
        .collect()
}

async fn read(boot: &Boot, args: Value) -> Value {
    call_tool(boot, TOOL_REPORT_READ, spec_identity(boot), args)
        .await
        .expect("spec can read the report")
}

async fn overwrite_report_payload_cache(boot: &Boot, payload: Value) {
    let card_id = boot.report_card_id.to_string();
    let payload = serde_json::to_string(&payload).expect("serialize stale payload cache");
    calm_server::db::write_in_tx_typed(boot.repo.as_ref(), move |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE cards SET payload = ?1 WHERE id = ?2")
                .bind(payload)
                .bind(card_id)
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
    .expect("simulate a stale payload cache from a pre-gate binary");
}

/// Seed a two-block body through the legacy write tool.
async fn seed_two_blocks(boot: &Boot) -> Vec<(String, u64)> {
    call_tool(
        boot,
        TOOL_REPORT_WRITE,
        spec_identity(boot),
        json!({
            "body": "# A\n\nalpha\n\n# B\n\nbeta\n",
            "summary": "seeded",
            "message": "seed two blocks",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("seed write");
    let index = index_of(&read(boot, json!({})).await);
    assert_eq!(index.len(), 2, "two H1 sections → two blocks");
    index
}

#[tokio::test]
async fn block_write_invalidates_previously_read_whole_document_revision() {
    let boot = boot().await;
    let before = read(&boot, json!({})).await;
    assert_eq!(before["docRev"], 0);
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({"kind": "prose", "payload": {"markdown": "# Added\n"}}),
    )
    .await
    .unwrap();

    let conflict = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({"body": "# stale rewrite\n", "if_doc_rev": 0}),
    )
    .await
    .unwrap_err();
    assert_eq!(conflict.code, RPC_REV_CONFLICT);
    assert!(conflict.message.contains("current doc_rev is 1"));
    let after = read(&boot, json!({})).await;
    assert_eq!(after["docRev"], 1);
    assert_ne!(after["body"], "# stale rewrite\n");
}

#[tokio::test]
async fn block_revision_cannot_be_used_as_a_whole_document_anchor() {
    let boot = boot().await;
    let created = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({"kind": "prose", "markdown": "# Added\n"}),
    )
    .await
    .unwrap();
    assert_eq!(created["rev"], 1);
    assert_eq!(created["docRev"], 1);

    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "# accidental overwrite\n",
            "message": "wrong revision domain",
            "if_rev": 1
        }),
    )
    .await
    .expect_err("a block if_rev must not satisfy the document anchor");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("if_doc_rev"));
    assert_ne!(
        read(&boot, json!({})).await["body"],
        "# accidental overwrite\n"
    );
}

// ---------------------------------------------------------------------------
// blocks.kinds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kinds_returns_all_five_schemas() {
    let boot = boot().await;
    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_KINDS,
        spec_identity(&boot),
        json!({}),
    )
    .await
    .expect("kinds succeeds");
    let kinds = out
        .get("kinds")
        .and_then(Value::as_array)
        .expect("kinds array");
    let names: Vec<&str> = kinds
        .iter()
        .map(|k| k.get("kind").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(names, ["prose", "chart.candles", "table", "app", "task"]);
    for kind in kinds {
        assert_eq!(
            kind.pointer("/schema/type").and_then(Value::as_str),
            Some("object"),
            "{kind}"
        );
        assert!(
            kind.get("usage")
                .and_then(Value::as_str)
                .is_some_and(|usage| !usage.is_empty()),
            "{kind}"
        );
    }
    let prose = &kinds[0];
    assert_eq!(
        prose.pointer("/schema/required/0").and_then(Value::as_str),
        Some("markdown"),
    );
    let chart = &kinds[1];
    assert_eq!(
        chart.pointer("/schema/required").unwrap(),
        &json!(["symbol", "candles"]),
    );
    let task = &kinds[4];
    assert_eq!(
        task.pointer("/schema/properties/context/$ref"),
        Some(&json!("#/$defs/contextValue"))
    );
    assert_eq!(
        task.pointer("/schema/$defs/contextValue/oneOf/0/maxLength"),
        Some(&json!(calm_types::report_blocks::MAX_STRING_CHARS))
    );
    assert!(
        task["usage"]
            .as_str()
            .is_some_and(|usage| usage.contains("context") && usage.contains("2048"))
    );
    assert_eq!(
        chart
            .pointer("/schema/properties/candles/minItems")
            .and_then(Value::as_u64),
        Some(2),
    );
    assert!(
        chart
            .get("usage")
            .and_then(Value::as_str)
            .unwrap()
            .contains("blocks.upsert"),
        "usage carries a minimal example"
    );
    let table = &kinds[2];
    assert_eq!(
        table.pointer("/schema/required").unwrap(),
        &json!(["columns", "rows"]),
    );
    let app = &kinds[3];
    assert_eq!(app.pointer("/schema/required").unwrap(), &json!(["src"]));
    assert_eq!(
        app.pointer("/schema/properties/height/maximum")
            .and_then(Value::as_u64),
        Some(2000),
    );
    let task = &kinds[4];
    assert_eq!(
        task.pointer("/schema/additionalProperties"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        task.pointer("/schema/properties/declared_by/enum"),
        Some(&json!(["spec"]))
    );

    // #960 PR3 review round 1: advertised limits mirror the Rust
    // validator (calm_types::report_blocks::kinds) so agents can
    // self-limit before the round-trip.
    assert_eq!(
        chart
            .pointer("/schema/properties/candles/maxItems")
            .and_then(Value::as_u64),
        Some(5000),
    );
    assert_eq!(
        chart
            .pointer("/schema/properties/symbol/maxLength")
            .and_then(Value::as_u64),
        Some(2048),
    );
    assert_eq!(
        table
            .pointer("/schema/properties/columns/maxItems")
            .and_then(Value::as_u64),
        Some(32),
    );
    assert_eq!(
        table
            .pointer("/schema/properties/rows/maxItems")
            .and_then(Value::as_u64),
        Some(500),
    );
    assert!(
        table
            .pointer("/schema/properties/rows/items/description")
            .and_then(Value::as_str)
            .is_some_and(|d| d.contains("declared column") && d.contains("PE")),
        "row-key rule + counter-example in the description"
    );
    assert_eq!(
        table
            .pointer("/schema/properties/rows/items/additionalProperties/maxLength")
            .and_then(Value::as_u64),
        Some(2048),
        "string cell values advertise the 2048-char cap"
    );
    assert_eq!(
        app.pointer("/schema/properties/src/pattern")
            .and_then(Value::as_str),
        Some("^/(?![/\\\\])[^\\\\]*$"),
    );
    assert!(
        app.pointer("/schema/properties/src/description")
            .and_then(Value::as_str)
            .is_some_and(|d| d.contains("NOT accepted")),
        "src description forbids full URLs"
    );
}

#[tokio::test]
async fn kinds_refuses_worker() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_KINDS,
        worker_identity(&boot),
        json!({}),
    )
    .await
    .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
}

// ---------------------------------------------------------------------------
// read: blocks index + markers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_returns_blocks_index_and_clean_text_by_default() {
    let boot = boot().await;
    let out = read(&boot, json!({})).await;
    assert_eq!(out.get("text").and_then(Value::as_str), Some(SEED_BODY));
    assert_eq!(
        out.get("body").and_then(Value::as_str),
        Some(SEED_BODY),
        "legacy `body` alias carries the same value as `text`",
    );
    assert!(
        !out["text"].as_str().unwrap().contains("<!-- neige:"),
        "default read output must be marker-free",
    );
    let index = index_of(&out);
    assert_eq!(index.len(), 1);
    assert!(index[0].0.starts_with("b_"), "id = {}", index[0].0);
    assert_eq!(index[0].1, 1);
    assert_eq!(
        out.pointer("/blocks/0/kind").and_then(Value::as_str),
        Some("prose"),
    );
}

#[tokio::test]
async fn read_with_markers_injects_marker_lines_but_never_stores_them() {
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    let out = read(&boot, json!({ "with_markers": true })).await;
    let text = out.get("text").and_then(Value::as_str).unwrap();
    assert_eq!(
        text,
        format!(
            "<!-- neige:{} -->\n# A\n\nalpha\n\n<!-- neige:{} -->\n# B\n\nbeta\n",
            ids[0].0, ids[1].0
        ),
    );
    // Markers exist only in the read output — storage stays clean.
    let payload = current_payload(&boot).await;
    assert!(!payload.body.contains("<!-- neige:"));
    // And a plain read right after is clean too.
    let plain = read(&boot, json!({})).await;
    assert!(!plain["text"].as_str().unwrap().contains("<!-- neige:"));
}

// ---------------------------------------------------------------------------
// blocks.upsert
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upsert_new_block_appends_and_emits_both_events() {
    let boot = boot().await;
    let events = boot.ctx.events.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "kind": "prose", "markdown": "# 新块\n\ncontent\n" }),
    )
    .await
    .expect("upsert create succeeds");
    let id = out
        .get("id")
        .and_then(Value::as_str)
        .expect("id")
        .to_string();
    assert!(id.starts_with("b_"));
    assert_eq!(out.get("rev").and_then(Value::as_u64), Some(1));
    assert!(out.get("updated_at").and_then(Value::as_i64).is_some());
    assert_eq!(out.get("docRev").and_then(Value::as_u64), Some(1));

    // Dual-event invariant: exactly one CardUpdated + one
    // WaveReportEdited, flat projections, summary untouched.
    let envs = sub.await.expect("collector ok");
    assert_eq!(envs.len(), 2, "got {envs:?}");
    assert!(matches!(envs[0].event, Event::CardUpdated(_)));
    match &envs[1].event {
        Event::WaveReportEdited {
            summary_before,
            summary_after,
            body_before,
            body_after,
            ..
        } => {
            assert_eq!(body_before, SEED_BODY);
            assert_eq!(body_after, &format!("{SEED_BODY}# 新块\n\ncontent\n"));
            assert_eq!(
                summary_before, summary_after,
                "block ops never touch summary"
            );
        }
        other => panic!("expected WaveReportEdited, got {other:?}"),
    }

    // JSON cache mirrors the append.
    let payload = current_payload(&boot).await;
    assert_eq!(payload.body, format!("{SEED_BODY}# 新块\n\ncontent\n"));
    let blocks = payload.blocks.expect("blocks cache");
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[1].id, id);
    assert_eq!(blocks[1].rev, 1);
}

#[tokio::test]
async fn upsert_new_block_at_position_inserts() {
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "kind": "prose", "markdown": "# 首块\n\nfirst\n", "position": 0 }),
    )
    .await
    .expect("insert at 0 succeeds");
    let new_id = out.get("id").and_then(Value::as_str).unwrap().to_string();

    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(
        index.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        vec![new_id.as_str(), ids[0].0.as_str(), ids[1].0.as_str()],
    );
    let payload = current_payload(&boot).await;
    assert!(payload.body.starts_with("# 首块\n\nfirst\n# A\n"));

    // Out-of-range position is refused.
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "kind": "prose", "markdown": "x\n", "position": 99 }),
    )
    .await
    .expect_err("position out of range");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("out of range"), "msg = {err:?}");
}

#[tokio::test]
async fn upsert_replace_with_if_rev_bumps_rev() {
    let boot = boot().await;
    // The id handed out by `read` on a never-persisted card must be a
    // valid target: the CRDT seed mints the same deterministic ids.
    let index = index_of(&read(&boot, json!({})).await);
    let (id, rev) = index[0].clone();
    assert_eq!(rev, 1);

    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "id": id, "kind": "prose", "markdown": "# 概要\n\nrewritten\n", "if_rev": rev }),
    )
    .await
    .expect("replace with matching if_rev succeeds");
    assert_eq!(out.get("id").and_then(Value::as_str), Some(id.as_str()));
    assert_eq!(out.get("rev").and_then(Value::as_u64), Some(2));

    let payload = current_payload(&boot).await;
    assert_eq!(payload.body, "# 概要\n\nrewritten\n");
    assert_eq!(payload.summary, "", "summary untouched");
    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(index, vec![(id, 2)]);
}

#[tokio::test]
async fn upsert_replace_without_if_rev_is_invalid_params() {
    let boot = boot().await;
    let index = index_of(&read(&boot, json!({})).await);
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "id": index[0].0, "kind": "prose", "markdown": "x\n" }),
    )
    .await
    .expect_err("replace without if_rev must be rejected");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("if_rev"), "msg = {err:?}");
}

#[tokio::test]
async fn upsert_rev_conflict_returns_32001_and_writes_nothing() {
    let boot = boot().await;
    let index = index_of(&read(&boot, json!({})).await);
    let (id, rev) = index[0].clone();
    let before = current_payload(&boot).await;
    let mut rx = boot.ctx.events.subscribe();

    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "id": id, "kind": "prose", "markdown": "# stomp\n", "if_rev": rev + 41 }),
    )
    .await
    .expect_err("stale if_rev must conflict");
    assert_eq!(err.code, RPC_REV_CONFLICT, "err = {err:?}");
    assert!(err.message.contains("rev conflict"), "msg = {err:?}");
    assert!(
        err.message.contains(&format!("current rev is {rev}")),
        "msg = {err:?}",
    );
    assert!(
        err.message
            .contains(&format!("expected if_rev {}", rev + 41)),
        "msg = {err:?}",
    );

    // Nothing written…
    let after = current_payload(&boot).await;
    assert_eq!(after, before, "conflict must not write");
    // …and nothing emitted (tx aborted before the event append).
    let no_event = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(no_event.is_err(), "conflict emitted event: {no_event:?}");
}

#[tokio::test]
async fn upsert_rejects_unknown_kind_and_invalid_payloads() {
    let boot = boot().await;
    let before = current_payload(&boot).await;
    // Unknown kind.
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "kind": "metrics", "payload": {} }),
    )
    .await
    .expect_err("unknown kind must be rejected");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("unknown kind"), "msg = {err:?}");
    // Data kind without payload / with markdown.
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "kind": "chart.candles" }),
    )
    .await
    .expect_err("missing payload");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("payload"), "msg = {err:?}");
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "kind": "table", "markdown": "# nope\n", "payload": { "columns": [], "rows": [] } }),
    )
    .await
    .expect_err("markdown on a data kind");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(
        err.message.contains("only valid for kind=prose"),
        "msg = {err:?}"
    );
    // Schema violations carry field-level paths.
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "kind": "chart.candles", "payload": { "symbol": "0700.HK", "candles": [[1, 2, 3, 4, 5]], "range": "1y" } }),
    )
    .await
    .expect_err("schema-invalid chart payload");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("at least 2 candles"), "msg = {err:?}");
    assert!(
        err.message.contains("range: unknown field"),
        "msg = {err:?}"
    );
    for src in [
        "https://evil.example/x",
        // Backslash bypass (#960 PR3 review round 1): browsers
        // normalize `/\host` into a protocol-relative URL.
        "/\\evil.example/x",
        "/apps\\x",
    ] {
        let err = call_tool(
            &boot,
            TOOL_REPORT_BLOCKS_UPSERT,
            spec_identity(&boot),
            json!({ "kind": "app", "payload": { "src": src } }),
        )
        .await
        .expect_err("non-same-origin app src");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(err.message.contains("src"), "{src} → {err:?}");
    }
    // Oversized payload: limit named in the error (-32602 path).
    let candles: Vec<Value> = (0..5001i64).map(|i| json!([i, 1, 2, 0, 1])).collect();
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "kind": "chart.candles", "payload": { "symbol": "X", "candles": candles } }),
    )
    .await
    .expect_err("over-cap candles");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("limit is 5000"), "msg = {err:?}");
    // Nothing was written by any of the rejected calls.
    assert_eq!(current_payload(&boot).await, before);
}

#[tokio::test]
async fn upsert_prose_rejects_embedded_neige_fences() {
    let boot = boot().await;
    for markdown in [
        // Well-formed fence smuggled inside prose.
        "# A\n```neige-block app\n{\"src\": \"/x\"}\n```\n",
        // Typo'd fence (bad JSON) — must not silently persist as prose.
        "# A\n```neige-block app\nnot json\n```\n",
    ] {
        let err = call_tool(
            &boot,
            TOOL_REPORT_BLOCKS_UPSERT,
            spec_identity(&boot),
            json!({ "kind": "prose", "markdown": markdown }),
        )
        .await
        .expect_err("prose with embedded fence must be rejected");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(err.message.contains("neige-block"), "msg = {err:?}");
    }
}

#[tokio::test]
async fn upsert_refuses_worker() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        worker_identity(&boot),
        json!({ "kind": "prose", "markdown": "evil\n" }),
    )
    .await
    .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("Spec"), "msg = {err:?}");
}

// ---------------------------------------------------------------------------
// blocks.move
// ---------------------------------------------------------------------------

#[tokio::test]
async fn move_reorders_without_touching_rev() {
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        spec_identity(&boot),
        json!({ "id": ids[1].0, "to_index": 0, "if_rev": ids[1].1 }),
    )
    .await
    .expect("move succeeds");
    assert_eq!(out.get("rev").and_then(Value::as_u64), Some(ids[1].1));

    let payload = current_payload(&boot).await;
    assert_eq!(out["docRev"], payload.doc_rev);
    assert_eq!(payload.body, "# B\n\nbeta\n# A\n\nalpha\n\n");
    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(
        index,
        vec![(ids[1].0.clone(), ids[1].1), (ids[0].0.clone(), ids[0].1)],
        "ids swap position, revs untouched",
    );
}

#[tokio::test]
async fn move_rev_conflict_returns_32001_and_moves_nothing() {
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    let before = current_payload(&boot).await;
    let mut rx = boot.ctx.events.subscribe();

    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        spec_identity(&boot),
        json!({ "id": ids[1].0, "to_index": 0, "if_rev": ids[1].1 + 7 }),
    )
    .await
    .expect_err("stale if_rev must conflict");
    assert_eq!(err.code, RPC_REV_CONFLICT);
    assert!(err.message.contains("rev conflict"), "msg = {err:?}");
    assert_eq!(current_payload(&boot).await, before);
    let no_event = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(no_event.is_err(), "conflict emitted event: {no_event:?}");

    // Unknown id and out-of-range index are invalid-params, not conflicts.
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        spec_identity(&boot),
        json!({ "id": "b_nope", "to_index": 0 }),
    )
    .await
    .expect_err("unknown id");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        spec_identity(&boot),
        json!({ "id": ids[0].0, "to_index": 5 }),
    )
    .await
    .expect_err("index out of range");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
}

// ---------------------------------------------------------------------------
// blocks.delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_requires_if_rev_and_honors_it() {
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;

    // Missing if_rev → invalid_params.
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_DELETE,
        spec_identity(&boot),
        json!({ "id": ids[0].0 }),
    )
    .await
    .expect_err("delete without if_rev must be rejected");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("if_rev"), "msg = {err:?}");

    // Stale if_rev → -32001, nothing deleted.
    let before = current_payload(&boot).await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_DELETE,
        spec_identity(&boot),
        json!({ "id": ids[0].0, "if_rev": ids[0].1 + 1 }),
    )
    .await
    .expect_err("stale if_rev must conflict");
    assert_eq!(err.code, RPC_REV_CONFLICT);
    assert!(err.message.contains("rev conflict"), "msg = {err:?}");
    assert_eq!(current_payload(&boot).await, before);

    // Matching if_rev deletes.
    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_DELETE,
        spec_identity(&boot),
        json!({ "id": ids[0].0, "if_rev": ids[0].1 }),
    )
    .await
    .expect("delete succeeds");
    let payload = current_payload(&boot).await;
    assert_eq!(out["docRev"], payload.doc_rev);
    assert_eq!(payload.body, "# B\n\nbeta\n");
    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(index, vec![(ids[1].0.clone(), ids[1].1)]);
}

// ---------------------------------------------------------------------------
// write_markdown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_markdown_requires_if_doc_rev_and_maps_stale_revision_to_conflict() {
    let boot = boot().await;
    let missing = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({ "body": "# Missing rev\n" }),
    )
    .await
    .expect_err("write_markdown without if_doc_rev must be rejected");
    assert_eq!(missing.code, RpcError::INVALID_PARAMS);

    let first = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({ "body": "# First\n", "if_doc_rev": 0 }),
    )
    .await
    .expect("fresh revision succeeds");
    assert_eq!(first["docRev"], 1);

    let stale = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({ "body": "# Stale\n", "if_doc_rev": 0 }),
    )
    .await
    .expect_err("stale whole-document anchor must conflict");
    assert_eq!(stale.code, RPC_REV_CONFLICT);
    assert!(stale.message.contains("current doc_rev is 1"));
    assert_eq!(current_payload(&boot).await.body, "# First\n");
}

#[tokio::test]
async fn write_markdown_with_markers_reuses_ids_and_strips_them() {
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;

    let events = boot.ctx.events.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Round-trip the with_markers read, editing only block B's text.
    let body = format!(
        "<!-- neige:{} -->\n# A\n\nalpha\n\n<!-- neige:{} -->\n# B\n\nbeta edited\n",
        ids[0].0, ids[1].0
    );
    let out = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({ "body": body, "if_doc_rev": 1 }),
    )
    .await
    .expect("write_markdown succeeds");

    // Ids survive; only the edited block's rev bumps.
    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(
        index,
        vec![
            (ids[0].0.clone(), ids[0].1),
            (ids[1].0.clone(), ids[1].1 + 1)
        ],
    );

    // Hard assertion: markers never reach storage nor the event log.
    let payload = current_payload(&boot).await;
    assert_eq!(out["docRev"], payload.doc_rev);
    assert_eq!(payload.body, "# A\n\nalpha\n\n# B\n\nbeta edited\n");
    assert!(!payload.body.contains("<!-- neige:"));
    assert_eq!(payload.summary, "seeded", "omitted summary is preserved");
    let envs = sub.await.expect("collector ok");
    assert_eq!(envs.len(), 2, "got {envs:?}");
    assert!(matches!(envs[0].event, Event::CardUpdated(_)));
    match &envs[1].event {
        Event::WaveReportEdited {
            body_before,
            body_after,
            ..
        } => {
            assert!(
                !body_after.contains("<!-- neige:"),
                "body_after = {body_after:?}"
            );
            assert!(!body_before.contains("<!-- neige:"));
            assert_eq!(body_after, "# A\n\nalpha\n\n# B\n\nbeta edited\n");
        }
        other => panic!("expected WaveReportEdited, got {other:?}"),
    }
}

#[tokio::test]
async fn write_markdown_markers_make_duplicate_blocks_addressable() {
    // Two byte-identical blocks — undecidable without markers — must
    // resolve exactly when markers pin them (design §3.4 / §4).
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": "# A\nsame\n# A\nsame\n",
            "message": "seed duplicate blocks",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("seed write");
    let ids = index_of(&read(&boot, json!({})).await);
    assert_eq!(ids.len(), 2);

    // Swap the two identical blocks by marker; edit the (now) second.
    let body = format!(
        "<!-- neige:{} -->\n# A\nsame\n<!-- neige:{} -->\n# A\nsame edited\n",
        ids[1].0, ids[0].0
    );
    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({ "body": body, "if_doc_rev": 1 }),
    )
    .await
    .expect("write_markdown succeeds");

    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(index[0].0, ids[1].0, "marker pinned the swap");
    assert_eq!(index[0].1, ids[1].1, "identical content: rev holds");
    assert_eq!(index[1].0, ids[0].0);
    assert_eq!(index[1].1, ids[0].1 + 1, "edited content: rev+1");
}

#[tokio::test]
async fn write_markdown_without_markers_falls_back_to_lcs() {
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({
            "body": "# X\n\nbrand new\n\n# A\n\nalpha touched\n\n# B\n\nbeta\n",
            "summary": "restructured",
            "if_doc_rev": 1
        }),
    )
    .await
    .expect("write_markdown succeeds");

    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(index.len(), 3);
    assert_ne!(index[0].0, ids[0].0);
    assert_ne!(index[0].0, ids[1].0);
    assert_eq!(index[0].1, 1, "new block starts at rev 1");
    assert_eq!(index[1].0, ids[0].0, "edited A inherits its id via LCS");
    assert_eq!(index[1].1, ids[0].1 + 1);
    assert_eq!(index[2].0, ids[1].0, "unchanged B keeps id");
    assert_eq!(index[2].1, ids[1].1, "unchanged B keeps rev");
    assert_eq!(current_payload(&boot).await.summary, "restructured");
}

#[tokio::test]
async fn upsert_identical_content_keeps_rev_and_still_emits_events() {
    // #960 PR2 review: a byte-identical replace is idempotent — the
    // rev must NOT bump (a retried request would otherwise silently
    // invalidate the caller's if_rev anchor). The persist boundary
    // still runs: dual events fire with body_before == body_after.
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    let (id, rev) = ids[0].clone();

    let events = boot.ctx.events.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "id": id, "kind": "prose", "markdown": "# A\n\nalpha\n\n", "if_rev": rev }),
    )
    .await
    .expect("identical replace succeeds");
    assert_eq!(out.get("id").and_then(Value::as_str), Some(id.as_str()));
    assert_eq!(
        out.get("rev").and_then(Value::as_u64),
        Some(rev),
        "identical content: rev unchanged"
    );

    // Dual-event invariant holds even for the no-op write.
    let envs = sub.await.expect("collector ok");
    assert_eq!(envs.len(), 2, "got {envs:?}");
    assert!(matches!(envs[0].event, Event::CardUpdated(_)));
    match &envs[1].event {
        Event::WaveReportEdited {
            body_before,
            body_after,
            ..
        } => assert_eq!(body_before, body_after, "no-op write: bodies equal"),
        other => panic!("expected WaveReportEdited, got {other:?}"),
    }

    // The unchanged rev is still a valid anchor: a real edit with the
    // SAME if_rev succeeds and bumps to rev+1.
    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "id": id, "kind": "prose", "markdown": "# A\n\nalpha edited\n\n", "if_rev": rev }),
    )
    .await
    .expect("subsequent real edit succeeds with the same if_rev");
    assert_eq!(out.get("rev").and_then(Value::as_u64), Some(rev + 1));
    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(index[0], (id, rev + 1));
}

#[tokio::test]
async fn read_blocks_index_comes_from_crdt_truth_when_cache_missing() {
    // #960 PR2 review: when a v2 row's JSON `blocks` cache is missing
    // (dropped by a pre-#960 binary — design D8), `read` must serve
    // the index from the CRDT doc, not re-derive ids from the flat
    // body (re-derivation mints position-dependent ids that diverge
    // from the doc after a `blocks.move` — handing out dead targets).
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    // Move B to the front so the CRDT order/ids can no longer be
    // reproduced by deriving from the flat body.
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        spec_identity(&boot),
        json!({ "id": ids[1].0, "to_index": 0 }),
    )
    .await
    .expect("move succeeds");
    let truth = index_of(&read(&boot, json!({})).await);
    assert_eq!(truth[0].0, ids[1].0, "B moved to front");

    // Simulate the dropped cache: rewrite the payload without `blocks`.
    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .expect("report card row");
    let mut payload = card.payload.clone();
    payload
        .as_object_mut()
        .expect("payload object")
        .remove("blocks")
        .expect("blocks cache was present");
    overwrite_report_payload_cache(&boot, payload).await;

    // Read must serve the CRDT truth (same ids/revs/order as before).
    let out = read(&boot, json!({})).await;
    assert_eq!(index_of(&out), truth, "index comes from the CRDT doc");

    // And the handed-out id/rev is a live target: upsert succeeds.
    let (id, rev) = truth[0].clone();
    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "id": id, "kind": "prose", "markdown": "# B\n\nbeta v2\n", "if_rev": rev }),
    )
    .await
    .expect("id from CRDT-truth read is upsertable");
    assert_eq!(out.get("rev").and_then(Value::as_u64), Some(rev + 1));
}

#[tokio::test]
async fn read_serves_one_self_consistent_snapshot_when_cache_missing() {
    // #960 PR2 review round 2: when the JSON cache is unusable, EVERY
    // read field (summary, text, blocks) must come from the same CRDT
    // doc — never text from the stale payload.body with a block index
    // from the doc. Make payload.body/summary diverge hard from the
    // CRDT and assert the doc wins everywhere.
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_MOVE,
        spec_identity(&boot),
        json!({ "id": ids[1].0, "to_index": 0 }),
    )
    .await
    .expect("move succeeds");
    let crdt_body = "# B\n\nbeta\n# A\n\nalpha\n\n";
    let truth = index_of(&read(&boot, json!({})).await);

    // Stale cache row: body/summary rewritten, blocks dropped.
    overwrite_report_payload_cache(
        &boot,
        json!({
            "schemaVersion": 2,
            "summary": "STALE SUMMARY",
            "body": "STALE BODY\n",
        }),
    )
    .await;

    let out = read(&boot, json!({})).await;
    assert_eq!(
        out.get("text").and_then(Value::as_str),
        Some(crdt_body),
        "text comes from the CRDT projection, not the stale payload.body"
    );
    assert_eq!(
        out.get("summary").and_then(Value::as_str),
        Some("seeded"),
        "summary comes from the same doc, not the stale payload"
    );
    assert_eq!(index_of(&out), truth, "block index from the same doc");

    // with_markers: text is the concatenation of the SAME blocks the
    // index lists — flatten(blocks) == body self-consistency.
    let marked = read(&boot, json!({ "with_markers": true })).await;
    let text = marked.get("text").and_then(Value::as_str).unwrap();
    assert_eq!(
        text,
        format!(
            "<!-- neige:{} -->\n# B\n\nbeta\n<!-- neige:{} -->\n# A\n\nalpha\n\n",
            truth[0].0, truth[1].0
        ),
    );
    assert_eq!(index_of(&marked), truth);
}

// ---------------------------------------------------------------------------
// #960 PR3 — data kinds end-to-end + prose-shim stomp guard
// ---------------------------------------------------------------------------

const CHART_PAYLOAD_V1: &str = r#"{
    "symbol": "0700.HK",
    "period": "day",
    "candles": [[1719800000000, 371.2, 380.0, 370.0, 378.4, 12000000],
                [1719886400000, 378.4, 382.0, 375.0, 379.8, 9800000]],
    "overlays": ["ma20"]
}"#;

/// Upsert one chart block after the seed prose; returns `(id, rev)`.
async fn upsert_chart(boot: &Boot, payload: Value) -> (String, u64) {
    let out = call_tool(
        boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(boot),
        json!({ "kind": "chart.candles", "payload": payload }),
    )
    .await
    .expect("chart upsert succeeds");
    (
        out.get("id").and_then(Value::as_str).unwrap().to_string(),
        out.get("rev").and_then(Value::as_u64).unwrap(),
    )
}

#[tokio::test]
async fn upsert_chart_block_projects_canonical_fence_and_typed_payload() {
    let boot = boot().await;
    let payload: Value = serde_json::from_str(CHART_PAYLOAD_V1).unwrap();
    let (id, rev) = upsert_chart(&boot, payload.clone()).await;
    assert_eq!(rev, 1);

    // Flat body carries the canonical fence (no id/rev inside — D9).
    let stored = current_payload(&boot).await;
    let fence = calm_types::report_blocks::render_fence("chart.candles", &payload);
    assert_eq!(stored.body, format!("{SEED_BODY}{fence}"));
    assert!(!fence.contains(&id), "fence must not embed the block id");

    // JSON blocks cache mirrors the typed payload (what the frontend
    // zod schema will consume), not a `{ markdown }` wrapper.
    let blocks = stored.blocks.expect("blocks cache");
    assert_eq!(blocks[1].id, id);
    assert_eq!(blocks[1].kind, "chart.candles");
    assert_eq!(blocks[1].payload, payload);

    // read index reports the kind; with_markers text embeds the fence.
    let out = read(&boot, json!({ "with_markers": true })).await;
    assert_eq!(
        out.pointer("/blocks/1/kind").and_then(Value::as_str),
        Some("chart.candles"),
    );
    let text = out.get("text").and_then(Value::as_str).unwrap();
    assert!(text.contains(&fence), "marker read embeds the fence");
    assert!(text.contains(&format!("<!-- neige:{id} -->")));

    // Replacing with different params bumps rev and changes the body.
    let mut v2 = payload.clone();
    v2["overlays"] = json!(["ma20", "ma60"]);
    let out = call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        spec_identity(&boot),
        json!({ "id": id, "kind": "chart.candles", "payload": v2, "if_rev": rev }),
    )
    .await
    .expect("chart replace succeeds");
    assert_eq!(out.get("rev").and_then(Value::as_u64), Some(2));
}

#[tokio::test]
async fn chart_param_change_yields_a_distinct_body_for_observation_hashing() {
    // Hard acceptance (design §3.5 / B-5): two documents that differ
    // ONLY in a chart parameter must produce different flat bodies —
    // the dispatcher's SHA256 observation fingerprint is taken over
    // `body_after`, so byte-equality here would merge distinct states.
    let boot_a = boot().await;
    let boot_b = boot().await;
    let payload: Value = serde_json::from_str(CHART_PAYLOAD_V1).unwrap();
    upsert_chart(&boot_a, payload.clone()).await;
    let mut tweaked = payload;
    // One candle close price differs.
    tweaked["candles"][1][4] = json!(379.9);
    upsert_chart(&boot_b, tweaked).await;

    let body_a = current_payload(&boot_a).await.body;
    let body_b = current_payload(&boot_b).await.body;
    assert_ne!(body_a, body_b, "chart params must be observable in body");
}

#[tokio::test]
async fn write_and_edit_stomping_a_data_block_fail_32602_and_write_nothing() {
    let boot = boot().await;
    let payload: Value = serde_json::from_str(CHART_PAYLOAD_V1).unwrap();
    let (id, _) = upsert_chart(&boot, payload).await;
    let before = current_payload(&boot).await;
    let mut rx = boot.ctx.events.subscribe();

    // write: fence dropped.
    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({ "body": "# 概要\n\nprose only now\n", "message": "stomp", "if_doc_rev": before.doc_rev }),
    )
    .await
    .expect_err("write dropping the fence must fail");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains(&id), "msg = {err:?}");
    assert!(err.message.contains("blocks.upsert"), "guidance: {err:?}");

    // edit: old_string lands inside the fence JSON.
    let err = call_tool(
        &boot,
        TOOL_REPORT_EDIT,
        spec_identity(&boot),
        json!({ "old_string": "\"ma20\"", "new_string": "\"ma60\"", "message": "stomp", "if_doc_rev": before.doc_rev }),
    )
    .await
    .expect_err("edit inside the fence must fail");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains(&id), "msg = {err:?}");

    // Storage unchanged, zero events (tx aborted).
    assert_eq!(current_payload(&boot).await, before);
    let no_event = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_event.is_err(),
        "guarded write emitted event: {no_event:?}"
    );
}

#[tokio::test]
async fn write_preserving_the_fence_verbatim_passes_and_holds_id_rev() {
    let boot = boot().await;
    let payload: Value = serde_json::from_str(CHART_PAYLOAD_V1).unwrap();
    let (id, rev) = upsert_chart(&boot, payload.clone()).await;
    let fence = calm_types::report_blocks::render_fence("chart.candles", &payload);

    // Whole-document rewrite via the prose shim carrying the fence
    // through byte-for-byte: legal.
    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        spec_identity(&boot),
        json!({
            "body": format!("# 概要\n\nrewritten prose\n{fence}# 新节\n\ntail\n"),
            "message": "legal whole-document rewrite",
            "if_doc_rev": 1
        }),
    )
    .await
    .expect("fence-preserving write passes");

    let stored = current_payload(&boot).await;
    assert!(stored.body.contains(&fence));
    let blocks = stored.blocks.expect("blocks cache");
    let chart = blocks.iter().find(|b| b.id == id).expect("chart survives");
    assert_eq!(chart.kind, "chart.candles");
    assert_eq!(u64::from(chart.rev), rev, "untouched fence: rev holds");
    assert_eq!(chart.payload, payload);
}

#[tokio::test]
async fn write_markdown_edits_fence_params_with_rev_bump_and_rejects_bad_fences() {
    let boot = boot().await;
    let payload: Value = serde_json::from_str(CHART_PAYLOAD_V1).unwrap();
    let (id, rev) = upsert_chart(&boot, payload.clone()).await;
    let prose_index = index_of(&read(&boot, json!({})).await);
    let (prose_id, prose_rev) = prose_index[0].clone();
    let fence = calm_types::report_blocks::render_fence("chart.candles", &payload);

    // Malformed fence JSON: whole write rejected, nothing lands.
    let before = current_payload(&boot).await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({ "body": "# 概要\n```neige-block chart.candles\n{oops\n```\n", "if_doc_rev": before.doc_rev }),
    )
    .await
    .expect_err("malformed fence must reject the whole write");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("neige-block"), "msg = {err:?}");
    assert_eq!(current_payload(&boot).await, before);

    // Editing the fence JSON through write_markdown: that block gets
    // rev+1, the prose block is untouched.
    let edited = fence.replace("\"ma20\"", "\"ma20\", \"ma60\"");
    call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        spec_identity(&boot),
        json!({ "body": format!("{SEED_BODY}{edited}"), "if_doc_rev": before.doc_rev }),
    )
    .await
    .expect("fence-editing write_markdown passes");
    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(index[0], (prose_id, prose_rev), "prose untouched");
    assert_eq!(index[1].0, id, "fence keeps its id");
    assert_eq!(index[1].1, rev + 1, "edited fence: rev+1");
    let blocks = current_payload(&boot).await.blocks.expect("blocks cache");
    assert_eq!(blocks[1].payload["overlays"], json!(["ma20", "ma60"]));
}

#[tokio::test]
async fn write_markdown_refuses_worker() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE_MARKDOWN,
        worker_identity(&boot),
        json!({ "body": "evil\n" }),
    )
    .await
    .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
}
