//! `neige_report_commit` block ops + `write_markdown` integration coverage on the `mcp_track_report` fixture.

#![cfg(unix)]

use std::time::Duration;

use crate::mcp_track_report::{
    Boot, assistant_identity, boot, call_tool, collect_n, planner_identity, read_then_commit,
    read_then_write_markdown, upsert_block, worker_identity,
};
use calm_server::event::Event;
use calm_server::mcp_server::tools::track_report_blocks::{
    RPC_REV_CONFLICT, TOOL_REPORT_COMMIT, TOOL_REPORT_KINDS, TOOL_REPORT_WRITE,
};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::track_report::TrackReportPayload;
use serde_json::{Value, json};

const TOOL_REPORT_READ: &str = "neige_report_read";
/// The birth body, read at runtime rather than re-transcribed.
fn seed_body() -> &'static str {
    static BODY: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| TrackReportPayload::initial().body);
    &BODY
}

/// Position of the block whose text starts with `head`, within the block index a `neige_report_read` just returned.
fn position_of_block_starting_with(read_out: &Value, head: &str) -> usize {
    let text = read_out["text"].as_str().expect("read returns text");
    calm_types::report_blocks::split_body(text)
        .iter()
        .position(|s| s.raw.starts_with(head))
        .unwrap_or_else(|| panic!("no block starts with {head:?} in {text:?}"))
}

/// Current report payload straight from the card row.
async fn current_payload(boot: &Boot) -> TrackReportPayload {
    let card = boot
        .repo
        .card_get(boot.report_card_id.as_str())
        .await
        .unwrap()
        .expect("report card row");
    serde_json::from_value(card.payload).expect("payload deserializes")
}

/// `[(id, rev)]` from a `neige_report_read` response's blocks index.
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
    call_tool(boot, TOOL_REPORT_READ, planner_identity(boot), args)
        .await
        .expect("planner can read the report")
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

/// Seed a two-block body through the whole-document write.
async fn seed_two_blocks(boot: &Boot) -> Vec<(String, u64)> {
    read_then_write_markdown(
        boot,
        planner_identity(boot),
        json!({
            "body": "# A\n\nalpha\n\n# B\n\nbeta\n",
            "summary": "seeded",
            "message": "seed two blocks"
        }),
    )
    .await
    .expect("seed write");
    let index = index_of(&read(boot, json!({})).await);
    assert_eq!(index.len(), 2, "two H1 sections → two blocks");
    index
}

#[tokio::test]
async fn another_writer_invalidates_a_previously_read_whole_document() {
    let boot = boot().await;
    let before = read(&boot, json!({})).await;
    assert_eq!(before["docRev"], 0);
    upsert_block(
        &boot,
        assistant_identity(&boot),
        json!({"kind": "prose", "payload": {"markdown": "# Added\n"}}),
    )
    .await
    .unwrap();

    let conflict = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({"body": "# stale rewrite\n"}),
    )
    .await
    .unwrap_err();
    assert_eq!(conflict.code, RPC_REV_CONFLICT);
    assert!(conflict.message.contains("current doc_rev is 1"));
    let after = read(&boot, json!({})).await;
    assert_eq!(after["docRev"], 1);
    assert_ne!(after["text"], "# stale rewrite\n");
}

/// #1883: the agent writes take no revisions; a caller still sending one is refused, never ignored.
#[tokio::test]
async fn removed_revision_params_are_refused_as_unknown_parameters() {
    let boot = boot().await;
    let index = index_of(&read(&boot, json!({})).await);
    let before = current_payload(&boot).await;
    let cases = [
        (
            TOOL_REPORT_COMMIT,
            json!({ "if_doc_rev": 0, "message": "m", "summary": "x" }),
            "unknown key `if_doc_rev`",
        ),
        (
            TOOL_REPORT_COMMIT,
            json!({ "message": "m", "ops": [
                { "op": "upsert", "id": index[1].0, "if_rev": index[1].1, "kind": "prose", "markdown": "# x\n" }
            ] }),
            "ops[0]: unknown key `if_rev`",
        ),
        (
            TOOL_REPORT_WRITE,
            json!({ "body": "# overwrite\n", "if_doc_rev": 0 }),
            "unknown key `if_doc_rev`",
        ),
        (
            TOOL_REPORT_WRITE,
            json!({ "body": "# overwrite\n", "if_rev": 1 }),
            "unknown key `if_rev`",
        ),
    ];
    for (tool, args, needle) in cases {
        let err = call_tool(&boot, tool, planner_identity(&boot), args)
            .await
            .expect_err("a revision parameter must be refused");
        assert_eq!(err.code, RpcError::INVALID_PARAMS, "{tool}: {err:?}");
        assert!(err.message.contains(needle), "{tool}: {err:?}");
    }
    assert_eq!(current_payload(&boot).await, before, "nothing was written");
}

#[tokio::test]
async fn kinds_returns_all_supported_schemas() {
    let boot = boot().await;
    let out = call_tool(&boot, TOOL_REPORT_KINDS, planner_identity(&boot), json!({}))
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
    assert_eq!(
        names,
        [
            "prose",
            "chart.candles",
            "chart.series",
            "table",
            "app",
            "task",
            "preview",
            "view"
        ]
    );
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
    let task = &kinds[5];
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
            .contains(r#"{ "op": "upsert", "kind": "chart.candles""#),
        "usage carries a minimal example"
    );
    let series = &kinds[2];
    assert_eq!(
        series.pointer("/schema/required").unwrap(),
        &json!(["source", "series"]),
    );
    assert_eq!(
        series.pointer("/schema/additionalProperties"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        series
            .pointer("/schema/properties/series/maxItems")
            .and_then(Value::as_u64),
        Some(calm_types::report_blocks::MAX_CHART_SERIES as u64),
    );
    assert_eq!(
        series
            .pointer("/schema/properties/series/items/pattern")
            .and_then(Value::as_str),
        Some("^[A-Z]{2,8}:[A-Za-z0-9._-]{1,32}$"),
    );
    assert_eq!(
        series
            .pointer("/schema/properties/as_of/pattern")
            .and_then(Value::as_str),
        Some("^\\d{4}-\\d{2}-\\d{2}$"),
    );
    assert_eq!(
        series.pointer("/schema/properties/view/enum").unwrap(),
        &json!(["line", "normalized", "bar", "candles"]),
    );
    assert!(
        series["usage"]
            .as_str()
            .is_some_and(|usage| usage.contains("NOT inlined") && usage.contains("as_of")),
        "chart.series usage says the data is named, not carried: {series}"
    );
    assert!(
        !chart["usage"]
            .as_str()
            .unwrap()
            .contains("no market-data source"),
        "chart.candles no longer claims the kernel cannot resolve market data"
    );
    assert!(
        chart["usage"].as_str().unwrap().contains("chart.series"),
        "chart.candles usage points at chart.series"
    );
    let table = &kinds[3];
    // Assert each branch's `required` AND its `not`: a `oneOf` listing only required keys would admit
    // the payload the kernel's validator rejects.
    assert_eq!(
        table.pointer("/schema/oneOf/0/required").unwrap(),
        &json!(["columns", "rows"]),
    );
    assert_eq!(
        table.pointer("/schema/oneOf/0/not/required").unwrap(),
        &json!(["source"]),
    );
    assert_eq!(
        table.pointer("/schema/oneOf/1/required").unwrap(),
        &json!(["source"]),
    );
    assert_eq!(
        table.pointer("/schema/oneOf/1/not/anyOf").unwrap(),
        &json!([
            { "required": ["columns"] },
            { "required": ["rows"] },
            { "required": ["highlight"] },
        ]),
    );
    // The live `source` pattern is the schema's copy of `report_blocks::validate_live_source`; pin it so they cannot drift.
    assert_eq!(
        table
            .pointer("/schema/properties/source/pattern")
            .and_then(Value::as_str),
        Some("^neige://plugin/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$"),
    );
    let app = &kinds[4];
    assert_eq!(app.pointer("/schema/required").unwrap(), &json!(["src"]));
    assert_eq!(
        app.pointer("/schema/properties/height/maximum")
            .and_then(Value::as_u64),
        Some(2000),
    );
    let preview = &kinds[6];
    assert_eq!(
        preview.pointer("/schema/required").unwrap(),
        &json!(["key"])
    );
    assert_eq!(
        preview.pointer("/schema/additionalProperties"),
        Some(&Value::Bool(false))
    );
    // The key pattern is `neige_preview_register`'s; the path pattern is the `app` block's `src`.
    assert_eq!(
        preview.pointer("/schema/properties/key/pattern"),
        Some(&json!("^[a-z0-9][a-z0-9_-]{0,63}$"))
    );
    assert_eq!(
        preview.pointer("/schema/properties/path/pattern"),
        app.pointer("/schema/properties/src/pattern")
    );
    assert_eq!(
        preview
            .pointer("/schema/properties/height/minimum")
            .and_then(Value::as_u64),
        Some(120),
    );
    let task = &kinds[5];
    assert_eq!(
        task.pointer("/schema/additionalProperties"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        task.pointer("/schema/properties/declared_by/enum"),
        Some(&json!(["spec", "user"]))
    );

    // Advertised limits mirror the Rust validator (`calm_types::report_blocks::kinds`).
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
    let err = call_tool(&boot, TOOL_REPORT_KINDS, worker_identity(&boot), json!({}))
        .await
        .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
}

#[tokio::test]
async fn read_returns_blocks_index_and_clean_text_by_default() {
    let boot = boot().await;
    let out = read(&boot, json!({})).await;
    assert_eq!(out.get("text").and_then(Value::as_str), Some(seed_body()));
    assert!(
        out.get("body").is_none(),
        "#1727 S2: the legacy `body` alias is gone; got {out}",
    );
    // `<!-- neige:b_` is the marker shape (`marker_line_id`); the birth body's first line is the
    // `<!-- neige:contract … -->` header, which shares the `neige:` namespace but is document content, not a marker.
    assert!(
        !out["text"].as_str().unwrap().contains("<!-- neige:b_"),
        "default read output must be marker-free",
    );
    let index = index_of(&out);
    // The literal 5 is deliberate: the only end-to-end pin of the birth block count.
    assert_eq!(
        index.len(),
        5,
        "birth report is 1 contract block + 4 sections: {index:?}"
    );
    for (id, rev) in &index {
        assert!(id.starts_with("b_"), "id = {id}");
        assert_eq!(*rev, 1);
    }
    for i in 0..5 {
        assert_eq!(
            out.pointer(&format!("/blocks/{i}/kind"))
                .and_then(Value::as_str),
            Some("prose"),
        );
    }
    // The maintenance contract leads the document and closes before the first H1, which makes block 0 the contract rather than a section.
    let text = out["text"].as_str().unwrap();
    assert!(
        text.starts_with(calm_types::report_contract::HEADER_OPEN),
        "the birth body must lead with the contract header line (#1635 D2): {text:?}"
    );
    assert!(
        text.contains("<!-- 报告维护契约"),
        "the birth body must carry the maintenance contract: {text:?}"
    );
    let first_h1 = text.find("\n# ").expect("skeleton has H1 sections") + 1;
    assert!(
        text[..first_h1].ends_with("-->\n\n"),
        "the contract must close before the first H1: {:?}",
        &text[..first_h1]
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
    let plain = read(&boot, json!({})).await;
    assert!(!plain["text"].as_str().unwrap().contains("<!-- neige:"));
}

#[tokio::test]
async fn upsert_new_block_appends_and_emits_both_events() {
    let boot = boot().await;
    let events = boot.ctx.events.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let out = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "kind": "prose", "markdown": "# 新块\n\ncontent\n"}),
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

    let envs = sub.await.expect("collector ok");
    assert_eq!(envs.len(), 2, "got {envs:?}");
    assert!(matches!(envs[0].event, Event::CardUpdated(_)));
    match &envs[1].event {
        Event::TrackReportEdited {
            summary_before,
            summary_after,
            body_before,
            body_after,
            ..
        } => {
            assert_eq!(body_before, seed_body());
            assert_eq!(body_after, &format!("{}# 新块\n\ncontent\n", seed_body()));
            assert_eq!(
                summary_before, summary_after,
                "block ops never touch summary"
            );
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }

    let payload = current_payload(&boot).await;
    assert_eq!(payload.body, format!("{}# 新块\n\ncontent\n", seed_body()));
    let blocks = payload.blocks.expect("blocks cache");
    assert_eq!(blocks.len(), 6, "5 skeleton blocks + the appended one");
    let appended = blocks
        .iter()
        .find(|b| b.id == id)
        .expect("appended block is in the cache");
    assert_eq!(appended.rev, 1);
}

#[tokio::test]
async fn upsert_new_block_at_position_inserts() {
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    let out = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "kind": "prose", "markdown": "# 首块\n\nfirst\n", "position": 0}),
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

    let err = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "kind": "prose", "markdown": "x\n", "position": 99}),
    )
    .await
    .expect_err("position out of range");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("out of range"), "msg = {err:?}");
}

#[tokio::test]
async fn upsert_replace_bumps_rev() {
    let boot = boot().await;
    // The id `read` hands out on a never-persisted card must be a valid target: the CRDT seed mints the same deterministic ids.
    let read_out = read(&boot, json!({})).await;
    let index = index_of(&read_out);
    let at = position_of_block_starting_with(&read_out, "# 概要");
    let (id, rev) = index[at].clone();
    assert_eq!(rev, 1);

    let out = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "id": id, "kind": "prose", "markdown": "# 概要\n\nrewritten\n"}),
    )
    .await
    .expect("replace of a block this session read succeeds");
    assert_eq!(out.get("id").and_then(Value::as_str), Some(id.as_str()));
    assert_eq!(out.get("rev").and_then(Value::as_u64), Some(2));

    let payload = current_payload(&boot).await;
    let expected: String = calm_types::report_blocks::split_body(seed_body())
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if i == at {
                "# 概要\n\nrewritten\n".to_string()
            } else {
                s.raw.clone()
            }
        })
        .collect();
    assert_eq!(payload.body, expected);
    assert_eq!(payload.summary, "", "summary untouched");
    let after = index_of(&read(&boot, json!({})).await);
    assert_eq!(
        after.len(),
        5,
        "replacing a block does not change the count"
    );
    assert_eq!(after[at], (id, 2));
}

#[tokio::test]
async fn upsert_of_a_block_another_writer_replaced_returns_32001_and_writes_nothing() {
    let boot = boot().await;
    let index = index_of(&read(&boot, json!({})).await);
    let (id, rev) = index[1].clone();
    upsert_block(
        &boot,
        assistant_identity(&boot),
        json!({ "id": id, "kind": "prose", "markdown": "# 概要\n\nassistant\n" }),
    )
    .await
    .expect("another session replaces the block");
    let before = current_payload(&boot).await;
    let mut rx = boot.ctx.events.subscribe();

    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({ "message": "stale", "ops": [
            { "op": "upsert", "id": id, "kind": "prose", "markdown": "# stomp\n" }
        ] }),
    )
    .await
    .expect_err("a block changed since this session's read must conflict");
    assert_eq!(err.code, RPC_REV_CONFLICT, "err = {err:?}");
    assert!(err.message.contains("rev conflict"), "msg = {err:?}");
    assert!(
        err.message.contains(&format!("current rev is {}", rev + 1)),
        "msg = {err:?}",
    );

    let after = current_payload(&boot).await;
    assert_eq!(after, before, "conflict must not write");
    let no_event = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(no_event.is_err(), "conflict emitted event: {no_event:?}");
}

#[tokio::test]
async fn upsert_rejects_unknown_kind_and_invalid_payloads() {
    let boot = boot().await;
    let before = current_payload(&boot).await;
    let err = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "kind": "metrics", "payload": {} }),
    )
    .await
    .expect_err("unknown kind must be rejected");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("unknown kind"), "msg = {err:?}");
    let err = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "kind": "chart.candles" }),
    )
    .await
    .expect_err("missing payload");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("payload"), "msg = {err:?}");
    let err = upsert_block(&boot, planner_identity(&boot), json!({ "kind": "table", "markdown": "# nope\n", "payload": { "columns": [], "rows": [] } }))
    .await
    .expect_err("markdown on a data kind");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(
        err.message.contains("only valid for kind=prose"),
        "msg = {err:?}"
    );
    let err = upsert_block(&boot, planner_identity(&boot), json!({ "kind": "chart.candles", "payload": { "symbol": "0700.HK", "candles": [[1, 2, 3, 4, 5]], "range": "1y" } }))
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
        // Backslash bypass: browsers normalize `/\host` into a protocol-relative URL.
        "/\\evil.example/x",
        "/apps\\x",
    ] {
        let err = upsert_block(
            &boot,
            planner_identity(&boot),
            json!({ "kind": "app", "payload": { "src": src } }),
        )
        .await
        .expect_err("non-same-origin app src");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(err.message.contains("src"), "{src} → {err:?}");
    }
    for (payload, needle) in [
        (json!({ "key": "Fe" }), "key: required"),
        (json!({ "key": "fe", "path": "//x" }), "path: must be"),
        (json!({ "key": "fe", "height": 50 }), "height"),
        (json!({ "key": "fe", "port": 4050 }), "port: unknown field"),
    ] {
        let err = upsert_block(
            &boot,
            planner_identity(&boot),
            json!({ "kind": "preview", "payload": payload }),
        )
        .await
        .expect_err("invalid preview payload");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(err.message.contains(needle), "{payload} → {err:?}");
    }
    let candles: Vec<Value> = (0..5001i64).map(|i| json!([i, 1, 2, 0, 1])).collect();
    let err = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "kind": "chart.candles", "payload": { "symbol": "X", "candles": candles } }),
    )
    .await
    .expect_err("over-cap candles");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("limit is 5000"), "msg = {err:?}");
    assert_eq!(current_payload(&boot).await, before);
}

#[tokio::test]
async fn upsert_prose_rejects_embedded_neige_fences() {
    let boot = boot().await;
    for markdown in [
        "# A\n```neige-block app\n{\"src\": \"/x\"}\n```\n",
        // Typo'd fence (bad JSON) — must not silently persist as prose.
        "# A\n```neige-block app\nnot json\n```\n",
    ] {
        let err = upsert_block(
            &boot,
            planner_identity(&boot),
            json!({ "kind": "prose", "markdown": markdown }),
        )
        .await
        .expect_err("prose with embedded fence must be rejected");
        assert_eq!(err.code, RpcError::INVALID_PARAMS);
        assert!(err.message.contains("neige-block"), "msg = {err:?}");
    }
}

#[tokio::test]
async fn commit_refuses_worker() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        worker_identity(&boot),
        json!({ "message": "m", "ops": [{ "op": "upsert", "kind": "prose", "markdown": "evil\n" }] }),
    )
    .await
    .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("Planner"), "msg = {err:?}");
}

#[tokio::test]
async fn move_reorders_without_touching_rev() {
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    let out = read_then_commit(
        &boot,
        planner_identity(&boot),
        json!([{"op": "move",  "id": ids[1].0, "to_index": 0}]),
    )
    .await
    .expect("move succeeds");

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

/// #1883: a rewrite drops every block its body does not carry, so it needs this session to have
/// seen the whole report at the current docRev.
#[tokio::test]
async fn write_markdown_needs_a_whole_read_at_the_current_doc_rev() {
    let boot = boot().await;
    let write = |body: &str| {
        call_tool(
            &boot,
            TOOL_REPORT_WRITE,
            planner_identity(&boot),
            json!({ "body": body }),
        )
    };
    let unread = write("# Unread\n").await.expect_err("no read at all");
    assert_eq!(unread.code, RpcError::INVALID_PARAMS);
    assert!(
        unread.message.contains("full neige_report_read"),
        "{unread:?}"
    );

    read(&boot, json!({})).await;
    let first = write("# First\n\none\n")
        .await
        .expect("a full read anchors it");
    assert_eq!(first["docRev"], 1);

    // Another writer moves the doc on; a read of one section there shows only part of it.
    upsert_block(
        &boot,
        assistant_identity(&boot),
        json!({ "kind": "prose", "markdown": "# Second\n\ntwo\n" }),
    )
    .await
    .expect("assistant appends");
    read(&boot, json!({ "select": { "sections": ["First"] } })).await;
    let partial = write("# First\n\nonly\n")
        .await
        .expect_err("a partial read at a newer docRev must not anchor a rewrite");
    assert_eq!(partial.code, RpcError::INVALID_PARAMS);
    assert!(
        partial.message.contains("full neige_report_read"),
        "{partial:?}"
    );
    assert!(current_payload(&boot).await.body.contains("# Second"));

    read(&boot, json!({ "with_markers": true })).await;
    // The session's own commit advances its whole read.
    call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({ "message": "m", "summary": "s" }),
    )
    .await
    .expect("own commit");
    let own = write("# First\n\nthree\n")
        .await
        .expect("an own commit keeps the whole read");
    assert_eq!(own["docRev"], 4);
    assert_eq!(current_payload(&boot).await.body, "# First\n\nthree\n");
}

/// #1883: an own `write_markdown` counts as read, the same rule as an own commit; a foreign write
/// in between still breaks the anchor.
#[tokio::test]
async fn an_own_write_markdown_counts_as_read_but_a_foreign_write_in_between_does_not() {
    let boot = boot().await;
    let commit_summary = |summary: &str| {
        call_tool(
            &boot,
            TOOL_REPORT_COMMIT,
            planner_identity(&boot),
            json!({ "message": "m", "summary": summary }),
        )
    };
    read(&boot, json!({})).await;
    let written = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({ "body": "# A\n\nalpha\n" }),
    )
    .await
    .expect("a full read anchors the rewrite");
    let out = commit_summary("s1")
        .await
        .expect("the own rewrite counts as read: no re-read before the summary");
    assert_eq!(
        out["docRev"].as_u64(),
        written["docRev"].as_u64().map(|r| r + 1)
    );

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({ "body": "# A\n\nalpha 2\n" }),
    )
    .await
    .expect("the own commit kept the whole read");
    upsert_block(
        &boot,
        assistant_identity(&boot),
        json!({ "kind": "prose", "markdown": "# B\n\nforeign\n" }),
    )
    .await
    .expect("another session writes");
    let err = commit_summary("s2")
        .await
        .expect_err("a foreign write since the rewrite");
    assert_eq!(err.code, RPC_REV_CONFLICT, "{err:?}");
    assert_eq!(current_payload(&boot).await.summary, "s1");
}

#[tokio::test]
async fn write_markdown_with_markers_reuses_ids_and_strips_them() {
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;

    let events = boot.ctx.events.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let body = format!(
        "<!-- neige:{} -->\n# A\n\nalpha\n\n<!-- neige:{} -->\n# B\n\nbeta edited\n",
        ids[0].0, ids[1].0
    );
    let out = read_then_write_markdown(&boot, planner_identity(&boot), json!({ "body": body}))
        .await
        .expect("write_markdown succeeds");

    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(
        index,
        vec![
            (ids[0].0.clone(), ids[0].1),
            (ids[1].0.clone(), ids[1].1 + 1)
        ],
    );

    let payload = current_payload(&boot).await;
    assert_eq!(out["docRev"], payload.doc_rev);
    assert_eq!(payload.body, "# A\n\nalpha\n\n# B\n\nbeta edited\n");
    assert!(!payload.body.contains("<!-- neige:"));
    assert_eq!(payload.summary, "seeded", "omitted summary is preserved");
    let envs = sub.await.expect("collector ok");
    assert_eq!(envs.len(), 2, "got {envs:?}");
    assert!(matches!(envs[0].event, Event::CardUpdated(_)));
    match &envs[1].event {
        Event::TrackReportEdited {
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
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }
}

#[tokio::test]
async fn write_markdown_markers_make_duplicate_blocks_addressable() {
    // Two byte-identical blocks are undecidable without markers; markers must pin them exactly.
    let boot = boot().await;
    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "# A\nsame\n# A\nsame\n",
            "message": "seed duplicate blocks"
        }),
    )
    .await
    .expect("seed write");
    let ids = index_of(&read(&boot, json!({})).await);
    assert_eq!(ids.len(), 2);

    let body = format!(
        "<!-- neige:{} -->\n# A\nsame\n<!-- neige:{} -->\n# A\nsame edited\n",
        ids[1].0, ids[0].0
    );
    read_then_write_markdown(&boot, planner_identity(&boot), json!({ "body": body}))
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
    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "# X\n\nbrand new\n\n# A\n\nalpha touched\n\n# B\n\nbeta\n",
            "summary": "restructured"
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
    // A byte-identical replace must not bump the rev: a retried request would otherwise invalidate the caller's block anchor.
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    let (id, rev) = ids[0].clone();

    let events = boot.ctx.events.clone();
    let sub = tokio::spawn(async move { collect_n(&events, 2).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let out = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "id": id, "kind": "prose", "markdown": "# A\n\nalpha\n\n"}),
    )
    .await
    .expect("identical replace succeeds");
    assert_eq!(out.get("id").and_then(Value::as_str), Some(id.as_str()));
    assert_eq!(
        out.get("rev").and_then(Value::as_u64),
        Some(rev),
        "identical content: rev unchanged"
    );

    let envs = sub.await.expect("collector ok");
    assert_eq!(envs.len(), 2, "got {envs:?}");
    assert!(matches!(envs[0].event, Event::CardUpdated(_)));
    match &envs[1].event {
        Event::TrackReportEdited {
            body_before,
            body_after,
            ..
        } => assert_eq!(body_before, body_after, "no-op write: bodies equal"),
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }

    let out = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "id": id, "kind": "prose", "markdown": "# A\n\nalpha edited\n\n"}),
    )
    .await
    .expect("a subsequent real edit succeeds");
    assert_eq!(out.get("rev").and_then(Value::as_u64), Some(rev + 1));
    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(index[0], (id, rev + 1));
}

#[tokio::test]
async fn read_blocks_index_comes_from_crdt_truth_when_cache_missing() {
    // With the JSON `blocks` cache missing, `read` must serve the index from the CRDT doc: re-deriving from
    // the flat body mints position-dependent ids that diverge after a `move`.
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    // Move B to the front so the CRDT order/ids can no longer be reproduced from the flat body.
    read_then_commit(
        &boot,
        planner_identity(&boot),
        json!([{"op": "move",  "id": ids[1].0, "to_index": 0}]),
    )
    .await
    .expect("move succeeds");
    let truth = index_of(&read(&boot, json!({})).await);
    assert_eq!(truth[0].0, ids[1].0, "B moved to front");

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

    let out = read(&boot, json!({})).await;
    assert_eq!(index_of(&out), truth, "index comes from the CRDT doc");

    let (id, rev) = truth[0].clone();
    let out = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "id": id, "kind": "prose", "markdown": "# B\n\nbeta v2\n"}),
    )
    .await
    .expect("id from CRDT-truth read is upsertable");
    assert_eq!(out.get("rev").and_then(Value::as_u64), Some(rev + 1));
}

#[tokio::test]
async fn read_serves_one_self_consistent_snapshot_when_cache_missing() {
    // When the JSON cache is unusable, every read field must come from the same CRDT doc — never text
    // from the stale `payload.body` with a block index from the doc.
    let boot = boot().await;
    let ids = seed_two_blocks(&boot).await;
    read_then_commit(
        &boot,
        planner_identity(&boot),
        json!([{"op": "move",  "id": ids[1].0, "to_index": 0}]),
    )
    .await
    .expect("move succeeds");
    let crdt_body = "# B\n\nbeta\n# A\n\nalpha\n\n";
    let truth = index_of(&read(&boot, json!({})).await);

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

const CHART_PAYLOAD_V1: &str = r#"{
    "symbol": "0700.HK",
    "period": "day",
    "candles": [[1719800000000, 371.2, 380.0, 370.0, 378.4, 12000000],
                [1719886400000, 378.4, 382.0, 375.0, 379.8, 9800000]],
    "overlays": ["ma20"]
}"#;

/// Upsert one chart block after the seed prose; returns `(id, rev)`.
async fn upsert_chart(boot: &Boot, payload: Value) -> (String, u64) {
    let out = upsert_block(
        boot,
        planner_identity(boot),
        json!({ "kind": "chart.candles", "payload": payload}),
    )
    .await
    .expect("chart upsert succeeds");
    (
        out.get("id").and_then(Value::as_str).unwrap().to_string(),
        out.get("rev").and_then(Value::as_u64).unwrap(),
    )
}

#[tokio::test]
async fn upsert_preview_block_round_trips_its_payload() {
    let boot = boot().await;
    let payload = json!({ "key": "fe", "title": "前端", "path": "/next/", "height": 720 });
    upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "kind": "preview", "payload": payload}),
    )
    .await
    .expect("preview upsert succeeds");
    let stored = current_payload(&boot).await;
    let fence = calm_types::report_blocks::render_fence("preview", &payload);
    assert_eq!(stored.body, format!("{}{fence}", seed_body()));
}

#[tokio::test]
async fn upsert_chart_block_projects_canonical_fence_and_typed_payload() {
    let boot = boot().await;
    let payload: Value = serde_json::from_str(CHART_PAYLOAD_V1).unwrap();
    let (id, rev) = upsert_chart(&boot, payload.clone()).await;
    assert_eq!(rev, 1);

    // Flat body carries the canonical fence (no id/rev inside).
    let stored = current_payload(&boot).await;
    let fence = calm_types::report_blocks::render_fence("chart.candles", &payload);
    assert_eq!(stored.body, format!("{}{fence}", seed_body()));
    assert!(!fence.contains(&id), "fence must not embed the block id");

    let blocks = stored.blocks.expect("blocks cache");
    let chart = blocks
        .iter()
        .find(|b| b.kind == "chart.candles")
        .expect("chart block is in the cache");
    assert_eq!(chart.id, id);
    assert_eq!(chart.payload, payload);

    let out = read(&boot, json!({ "with_markers": true })).await;
    assert_eq!(
        out["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b["kind"] == "chart.candles")
            .count(),
        1,
        "the read index reports the chart's kind: {out}"
    );
    let text = out.get("text").and_then(Value::as_str).unwrap();
    assert!(text.contains(&fence), "marker read embeds the fence");
    assert!(text.contains(&format!("<!-- neige:{id} -->")));

    let mut v2 = payload.clone();
    v2["overlays"] = json!(["ma20", "ma60"]);
    let out = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "id": id, "kind": "chart.candles", "payload": v2}),
    )
    .await
    .expect("chart replace succeeds");
    assert_eq!(out.get("rev").and_then(Value::as_u64), Some(2));
}

#[tokio::test]
async fn chart_param_change_yields_a_distinct_body_for_observation_hashing() {
    // Two documents that differ only in a chart parameter must produce different flat bodies: the
    // dispatcher's SHA256 observation fingerprint is taken over `body_after`.
    let boot_a = boot().await;
    let boot_b = boot().await;
    let payload: Value = serde_json::from_str(CHART_PAYLOAD_V1).unwrap();
    upsert_chart(&boot_a, payload.clone()).await;
    let mut tweaked = payload;
    tweaked["candles"][1][4] = json!(379.9);
    upsert_chart(&boot_b, tweaked).await;

    let body_a = current_payload(&boot_a).await.body;
    let body_b = current_payload(&boot_b).await.body;
    assert_ne!(body_a, body_b, "chart params must be observable in body");
}

#[tokio::test]
async fn write_markdown_preserving_the_fence_verbatim_passes_and_holds_id_rev() {
    let boot = boot().await;
    let payload: Value = serde_json::from_str(CHART_PAYLOAD_V1).unwrap();
    let (id, rev) = upsert_chart(&boot, payload.clone()).await;
    let fence = calm_types::report_blocks::render_fence("chart.candles", &payload);

    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": format!("# 概要\n\nrewritten prose\n{fence}# 新节\n\ntail\n"),
            "message": "legal whole-document rewrite"
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
    let read_out = read(&boot, json!({})).await;
    let prose_index = index_of(&read_out);
    let summary_at = position_of_block_starting_with(&read_out, "# 概要");
    let (prose_id, prose_rev) = prose_index[summary_at].clone();
    let fence = calm_types::report_blocks::render_fence("chart.candles", &payload);

    let before = current_payload(&boot).await;
    let err = read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({ "body": "# 概要\n```neige-block chart.candles\n{oops\n```\n"}),
    )
    .await
    .expect_err("malformed fence must reject the whole write");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(err.message.contains("neige-block"), "msg = {err:?}");
    assert_eq!(current_payload(&boot).await, before);

    let edited = fence.replace("\"ma20\"", "\"ma20\", \"ma60\"");
    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({ "body": format!("{}{edited}", seed_body())}),
    )
    .await
    .expect("fence-editing write_markdown passes");
    let index = index_of(&read(&boot, json!({})).await);
    assert_eq!(
        index[summary_at],
        (prose_id, prose_rev),
        "the summary section is untouched"
    );
    let fence_at = index
        .iter()
        .position(|(bid, _)| bid == &id)
        .expect("fence keeps its id");
    assert_eq!(index[fence_at].1, rev + 1, "edited fence: rev+1");
    let blocks = current_payload(&boot).await.blocks.expect("blocks cache");
    let chart = blocks
        .iter()
        .find(|b| b.id == id)
        .expect("chart block is in the cache");
    assert_eq!(chart.payload["overlays"], json!(["ma20", "ma60"]));
}

#[tokio::test]
async fn write_markdown_refuses_worker() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        worker_identity(&boot),
        json!({ "body": "evil\n" }),
    )
    .await
    .expect_err("worker must be denied");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
}

/// A schedulable planner task declaration: the full shape the projection materializes into a `tasks` row.
fn planner_task_payload(key: &str, goal: &str) -> Value {
    json!({
        "key": key, "kind": "codex", "goal": goal,
        "acceptance": format!("accept {key}"), "context": {"key": key},
        "cwd": format!("/{key}"), "depends_on": [], "priority": 3,
        "gate": {"steps": [{"name": "accept", "cmd": "true"}]},
        "declared_by": "spec", "ready": true
    })
}

/// Planner-declared live task, plus the `tasks` row key set it projects.
async fn seed_planner_task(boot: &Boot, key: &str) -> (String, u64) {
    let out = upsert_block(
        boot,
        planner_identity(boot),
        json!({
            "kind": "task",
            "payload": planner_task_payload(key, "build it")
        }),
    )
    .await
    .expect("planner declares a task block");
    (
        out["id"].as_str().expect("block id").to_string(),
        out["rev"].as_u64().expect("block rev"),
    )
}

async fn task_keys(boot: &Boot) -> Vec<String> {
    let pool = boot.repo.sqlite_pool().expect("sqlite pool");
    sqlx::query_scalar::<_, String>("SELECT key FROM tasks WHERE track_id = ?1 ORDER BY key")
        .bind(boot.track_id.as_str())
        .fetch_all(&pool)
        .await
        .expect("read task keys")
}

#[tokio::test]
async fn task_gate_rejects_kernel_cli_before_persisting_or_scheduling() {
    let boot = boot().await;
    let before = current_payload(&boot).await;
    let mut events = boot.ctx.events.subscribe();
    for cmd in [
        "neige cat plan/analyze/output",
        "neige 'cat' plan/analyze/output",
        "neige state>/dev/null",
        "neige 2>/dev/null state",
        "neige cat '--help'",
        "neige cat --help|cat",
        "neige cat>/dev/null --help",
    ] {
        let mut payload = planner_task_payload("analyze", "Analyze artifacts");
        payload["gate"]["steps"][0]["cmd"] = json!(cmd);
        let err = upsert_block(
            &boot,
            planner_identity(&boot),
            json!({"kind": "task", "payload": payload}),
        )
        .await
        .expect_err("credential-dependent gate must be rejected before expensive work");
        assert_eq!(err.code, -32602);
        assert!(err.message.contains("gate.steps[0].cmd"), "{err:?}");
        assert!(err.message.contains("NEIGE_MCP_SOCKET"), "{err:?}");
        assert!(err.message.contains("worker checkout"), "{err:?}");
        let after = current_payload(&boot).await;
        assert_eq!(after.doc_rev, before.doc_rev);
        assert_eq!(after.body, before.body);
        assert!(task_keys(&boot).await.is_empty());
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
}

#[tokio::test]
async fn task_gate_accepts_artifact_checks() {
    let boot = boot().await;
    for (index, cmd) in [
        "python3 -m unittest discover",
        "test -s artifacts/result.json",
    ]
    .iter()
    .enumerate()
    {
        let key = format!("check-{index}");
        let mut payload = planner_task_payload(&key, "Check local artifacts");
        payload["gate"]["steps"][0]["cmd"] = json!(cmd);
        upsert_block(
            &boot,
            planner_identity(&boot),
            json!({"kind": "task", "payload": payload}),
        )
        .await
        .expect("artifact verification must remain authorable");
        assert!(task_keys(&boot).await.contains(&key));
    }
}

#[tokio::test]
async fn write_markdown_cannot_silently_drop_a_planner_task_block() {
    let boot = boot().await;
    seed_planner_task(&boot, "build").await;
    let before = current_payload(&boot).await;
    assert!(
        before.body.contains("neige-block task"),
        "seeded body carries the task fence: {:?}",
        before.body
    );
    assert_eq!(task_keys(&boot).await, ["build"], "task row is projected");

    let err = read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({ "body": seed_body()}),
    )
    .await
    .expect_err("a body that drops the task fence must be rejected");
    assert_eq!(err.code, RpcError::INVALID_PARAMS);
    assert!(
        err.message.contains("must name it by id"),
        "the error must point at the delete op: {err:?}"
    );
    assert_eq!(
        current_payload(&boot).await,
        before,
        "the rejected write lands nothing"
    );
    assert_eq!(
        task_keys(&boot).await,
        ["build"],
        "the scheduling projection keeps the task"
    );
}

#[tokio::test]
async fn write_markdown_may_still_edit_a_task_fence_in_place() {
    let boot = boot().await;
    let (id, rev) = seed_planner_task(&boot, "build").await;
    let edited = calm_types::report_blocks::render_fence(
        "task",
        &planner_task_payload("build", "build it better"),
    );

    read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({ "body": format!("{}{edited}", seed_body())}),
    )
    .await
    .expect("editing the fence body in place stays legal");

    let blocks = current_payload(&boot).await.blocks.expect("blocks cache");
    let task = blocks.iter().find(|b| b.id == id).expect("task survives");
    assert_eq!(u64::from(task.rev), rev + 1, "edited fence: rev+1");
    assert_eq!(task.payload["goal"], "build it better");
    assert_eq!(task_keys(&boot).await, ["build"]);
}

#[tokio::test]
async fn planner_commit_delete_by_id_retires_its_live_task() {
    let boot = boot().await;
    let (id, _) = seed_planner_task(&boot, "build").await;
    assert_eq!(task_keys(&boot).await, ["build"]);

    read_then_commit(
        &boot,
        planner_identity(&boot),
        json!([{ "op": "delete", "id": id }]),
    )
    .await
    .expect("a delete naming the live task by id retires it");

    let payload = current_payload(&boot).await;
    assert!(
        !payload.body.contains("neige-block task"),
        "body drops the fence: {:?}",
        payload.body
    );
    assert!(task_keys(&boot).await.is_empty(), "task row is withdrawn");
}

#[tokio::test]
async fn write_markdown_changing_one_section_leaves_the_contract_byte_identical() {
    let boot = boot().await;

    let before_index = index_of(&read(&boot, json!({})).await);
    let before_body = current_payload(&boot).await.body;
    let contract_before = calm_types::report_blocks::split_body(&before_body)[0]
        .raw
        .clone();
    let summary_at = position_of_block_starting_with(&read(&boot, json!({})).await, "# 概要");

    let marked = read(&boot, json!({ "with_markers": true })).await;
    let text = marked["text"].as_str().unwrap();
    assert!(
        text.contains("<!-- 报告维护契约"),
        "the marker read hands the contract back to the agent verbatim"
    );
    let edited = text.replacen("# 概要\n", "# 概要\n\n当前进展一句话。\n", 1);
    assert_ne!(edited, text, "the fixture must actually change something");

    read_then_write_markdown(&boot, planner_identity(&boot), json!({ "body": edited}))
        .await
        .expect("marker-channel write passes");

    let after_body = current_payload(&boot).await.body;
    let after_slices = calm_types::report_blocks::split_body(&after_body);
    assert_eq!(after_slices.len(), 5, "block count is unchanged");
    assert_eq!(
        after_slices[0].raw, contract_before,
        "the maintenance contract must survive byte-identical"
    );

    let after_index = index_of(&read(&boot, json!({})).await);
    assert_eq!(
        after_index[0], before_index[0],
        "the contract block keeps both its id and its rev"
    );
    assert_eq!(
        after_index[summary_at].0, before_index[summary_at].0,
        "the edited section keeps its id"
    );
    assert_eq!(
        after_index[summary_at].1,
        before_index[summary_at].1 + 1,
        "the edited section gets rev+1"
    );
    assert!(after_body.contains("当前进展一句话。"));
}

#[path = "report_block_boundaries.rs"]
mod boundaries;

#[path = "report_block_upgrade.rs"]
mod upgrade;

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

fn commit_args(ops: Value) -> Value {
    json!({
        "message": "一次提交",
        "ops": ops
    })
}

#[tokio::test]
async fn commit_three_ops_and_summary_land_atomically_with_one_doc_rev_bump() {
    let boot = boot().await;
    let index = seed_two_blocks(&boot).await; // docRev 1, A@1, B@1
    let (a_id, a_rev) = index[0].clone();
    let (b_id, _) = index[1].clone();
    let before = current_payload(&boot).await;
    assert_eq!(before.doc_rev, 1);
    let mut rx = boot.ctx.events.subscribe();

    let out = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({
            "message": "三个块 + summary 一次提交",
            "summary": "新摘要",
            "ops": [
                { "op": "upsert", "id": a_id, "kind": "prose", "markdown": "# A\n\nalpha v2\n" },
                { "op": "delete", "id": b_id},
                { "op": "upsert", "kind": "prose", "markdown": "# C\n\ngamma\n", "position": 0 }
            ]
        }),
    )
    .await
    .expect("commit succeeds");

    assert_eq!(
        out["docRev"].as_u64(),
        Some(2),
        "one bump for three ops: {out}"
    );
    let blocks = out["blocks"].as_array().expect("blocks index");
    assert_eq!(blocks.len(), 2, "{out}");
    assert_eq!(blocks[0]["kind"], "prose");
    assert_eq!(blocks[0]["rev"].as_u64(), Some(1), "C is new: rev 1");
    assert_eq!(
        blocks[1]["id"].as_str(),
        Some(a_id.as_str()),
        "A keeps its id"
    );
    assert_eq!(
        blocks[1]["rev"].as_u64(),
        Some(a_rev + 1),
        "A rev bumped once"
    );
    assert!(blocks.iter().all(|b| b["id"] != json!(b_id)), "B deleted");

    let after = current_payload(&boot).await;
    assert_eq!(after.doc_rev, 2);
    assert_eq!(after.summary, "新摘要");
    assert_eq!(after.body, "# C\n\ngamma\n# A\n\nalpha v2\n");
    let read = read(&boot, json!({})).await;
    assert_eq!(index_of(&read).len(), 2);

    let envs = drain_events(&mut rx).await;
    let kinds: Vec<&str> = envs
        .iter()
        .map(|e| match &e.event {
            Event::TrackUpdated(_) => "track_updated",
            Event::CardUpdated(_) => "card_updated",
            Event::TrackReportEdited { .. } => "report_edited",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["card_updated", "report_edited"], "got {envs:?}");
    match &envs[1].event {
        Event::TrackReportEdited {
            agent_message,
            summary_before,
            summary_after,
            body_after,
            ..
        } => {
            assert_eq!(agent_message.as_deref(), Some("三个块 + summary 一次提交"));
            assert_eq!(summary_before, "seeded");
            assert_eq!(summary_after, "新摘要");
            assert_eq!(body_after, "# C\n\ngamma\n# A\n\nalpha v2\n");
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }
}

#[tokio::test]
async fn commit_stale_doc_rev_returns_32001_and_writes_nothing() {
    let boot = boot().await;
    let index = seed_two_blocks(&boot).await; // the planner read docRev 1
    let (a_id, _) = index[0].clone();
    upsert_block(
        &boot,
        assistant_identity(&boot),
        json!({ "kind": "prose", "markdown": "# C\n\nelsewhere\n" }),
    )
    .await
    .expect("another session moves the doc to docRev 2");
    let before = current_payload(&boot).await;
    let mut rx = boot.ctx.events.subscribe();

    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({
            "message": "stale anchor",
            "summary": "must not land",
            "ops": [
                { "op": "upsert", "id": a_id, "kind": "prose", "markdown": "# A\n\nstale\n" }
            ]
        }),
    )
    .await
    .expect_err("a summary over a doc changed since the read is a conflict");
    assert_eq!(err.code, RPC_REV_CONFLICT, "{err:?}");
    assert!(
        err.message.contains("document revision conflict"),
        "{err:?}"
    );

    let after = current_payload(&boot).await;
    assert_eq!(after.doc_rev, before.doc_rev);
    assert_eq!(after.body, before.body);
    assert_eq!(after.summary, before.summary);
    assert!(drain_events(&mut rx).await.is_empty(), "nothing emitted");
}

#[tokio::test]
async fn commit_stale_block_rev_in_second_op_rolls_back_the_first_op() {
    let boot = boot().await;
    let index = seed_two_blocks(&boot).await; // the planner read A@1, B@1
    let (a_id, a_rev) = index[0].clone();
    let (b_id, _) = index[1].clone();
    upsert_block(
        &boot,
        assistant_identity(&boot),
        json!({ "id": b_id, "kind": "prose", "markdown": "# B\n\nassistant\n" }),
    )
    .await
    .expect("another session replaces B");
    let before = current_payload(&boot).await;
    let mut rx = boot.ctx.events.subscribe();

    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        commit_args(json!([
            { "op": "upsert", "id": a_id, "kind": "prose", "markdown": "# A\n\nwould apply\n" },
            { "op": "delete", "id": b_id }
        ])),
    )
    .await
    .expect_err("op 2 is anchored by a stale read of B");
    assert_eq!(err.code, RPC_REV_CONFLICT, "{err:?}");
    assert!(
        err.message.contains("ops[1]"),
        "names the failing op: {err:?}"
    );

    let after = current_payload(&boot).await;
    assert_eq!(after.doc_rev, before.doc_rev);
    assert_eq!(after.body, before.body, "op 1 rolled back with the batch");
    let blocks = after.blocks.expect("blocks cache");
    assert_eq!(
        blocks.iter().find(|b| b.id == a_id).unwrap().rev,
        a_rev as u32
    );
    assert_eq!(blocks.len(), 2);
    assert!(drain_events(&mut rx).await.is_empty(), "nothing emitted");
}

#[tokio::test]
async fn commit_with_a_lifecycle_key_returns_32602_and_persists_no_content() {
    let boot = boot().await;
    let index = seed_two_blocks(&boot).await;
    let (a_id, _) = index[0].clone();
    let before = current_payload(&boot).await;
    let mut rx = boot.ctx.events.subscribe();

    let err = call_tool(&boot, TOOL_REPORT_COMMIT, planner_identity(&boot), json!({
            "message": "lifecycle is removed",
            "summary": "must not land",
            "lifecycle": "done",
            "ops": [
                { "op": "upsert", "id": a_id, "kind": "prose", "markdown": "# A\n\nmust not land\n" }
            ]
        }))
    .await
    .expect_err("a lifecycle key is refused");
    assert_eq!(err.code, -32602, "{err:?}");
    assert!(err.message.contains("`lifecycle` is removed"), "{err:?}");

    let after = current_payload(&boot).await;
    assert_eq!(after.doc_rev, before.doc_rev);
    assert_eq!(after.body, before.body);
    assert_eq!(after.summary, before.summary);
    assert!(drain_events(&mut rx).await.is_empty(), "nothing emitted");
}

#[tokio::test]
async fn commit_summary_only_with_empty_ops_bumps_doc_rev_and_keeps_blocks() {
    let boot = boot().await;
    let index = seed_two_blocks(&boot).await;
    let before = current_payload(&boot).await;
    let mut rx = boot.ctx.events.subscribe();

    let out = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({ "message": "只改摘要", "summary": "摘要 v2", "ops": [] }),
    )
    .await
    .expect("summary-only commit");
    assert_eq!(out["docRev"].as_u64(), Some(2));
    let blocks = out["blocks"].as_array().unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|b| (
                b["id"].as_str().unwrap().to_string(),
                b["rev"].as_u64().unwrap()
            ))
            .collect::<Vec<_>>(),
        index,
        "block ids and revs untouched"
    );
    let after = current_payload(&boot).await;
    assert_eq!(after.summary, "摘要 v2");
    assert_eq!(after.body, before.body);

    call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        json!({ "message": "再改摘要", "summary": "摘要 v3" }),
    )
    .await
    .expect("commit without ops key");
    assert_eq!(current_payload(&boot).await.summary, "摘要 v3");

    let envs = drain_events(&mut rx).await;
    assert_eq!(
        envs.len(),
        4,
        "two commits × (CardUpdated + TrackReportEdited): {envs:?}"
    );
    assert!(matches!(envs[0].event, Event::CardUpdated(_)));
    match &envs[1].event {
        Event::TrackReportEdited {
            summary_before,
            summary_after,
            body_before,
            body_after,
            ..
        } => {
            assert_eq!(summary_before, "seeded");
            assert_eq!(summary_after, "摘要 v2");
            assert_eq!(body_before, body_after);
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }
}

#[tokio::test]
async fn commit_rejects_empty_commits_and_malformed_ops_before_touching_the_doc() {
    let boot = boot().await;
    let index = seed_two_blocks(&boot).await;
    let (a_id, _) = index[0].clone();
    let before = current_payload(&boot).await;

    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "nothing to commit",
            json!({ "message": "empty" }),
            "nothing to commit",
        ),
        (
            "missing message",
            json!({ "summary": "x" }),
            "message must be non-empty",
        ),
        (
            "unknown op",
            commit_args(json!([{ "op": "rename", "id": a_id }])),
            "ops[0]: unknown op `rename`",
        ),
        (
            "move without to_index",
            commit_args(json!([{ "op": "move", "id": a_id }])),
            "ops[0]: missing `to_index`",
        ),
        (
            "prose smuggling a fence in op 2",
            commit_args(json!([
                { "op": "upsert", "id": a_id, "kind": "prose", "markdown": "# A\n\nok\n" },
                { "op": "upsert", "kind": "prose", "markdown": "# B\n\n```neige-block table\n{}\n```\n" }
            ])),
            "ops[1]:",
        ),
        (
            "too many ops",
            commit_args(Value::Array(vec![
                json!({ "op": "move", "id": a_id, "to_index": 0 });
                65
            ])),
            "at most 64",
        ),
    ];
    for (name, args, needle) in cases {
        let err = call_tool(&boot, TOOL_REPORT_COMMIT, planner_identity(&boot), args)
            .await
            .err()
            .unwrap_or_else(|| panic!("{name}: must be refused"));
        assert_eq!(err.code, -32602, "{name}: {err:?}");
        assert!(err.message.contains(needle), "{name}: {err:?}");
    }
    let after = current_payload(&boot).await;
    assert_eq!(
        after.doc_rev, before.doc_rev,
        "no refused case touched the doc"
    );
    assert_eq!(after.body, before.body);
}

#[tokio::test]
async fn upsert_and_write_markdown_carry_message_for_the_planner() {
    let boot = boot().await;
    let index = seed_two_blocks(&boot).await;
    let (a_id, a_rev) = index[0].clone();
    let mut rx = boot.ctx.events.subscribe();

    let out = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({
            "id": a_id, "kind": "prose", "markdown": "# A\n\nalpha v2\n",
            "message": "upsert 带说明"
        }),
    )
    .await
    .expect("planner upsert with message");
    assert_eq!(out["rev"].as_u64(), Some(a_rev + 1));
    let envs = drain_events(&mut rx).await;
    assert_eq!(envs.len(), 2, "{envs:?}");
    match &envs[1].event {
        Event::TrackReportEdited { agent_message, .. } => {
            assert_eq!(agent_message.as_deref(), Some("upsert 带说明"));
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }

    let out = read_then_write_markdown(
        &boot,
        planner_identity(&boot),
        json!({
            "body": "# A\n\nalpha v3\n\n# B\n\nbeta\n",
            "message": "write_markdown 带说明"
        }),
    )
    .await
    .expect("planner write_markdown with message");
    assert_eq!(out["docRev"].as_u64(), Some(3));
    let envs = drain_events(&mut rx).await;
    assert_eq!(envs.len(), 2, "{envs:?}");
    match &envs[1].event {
        Event::TrackReportEdited { agent_message, .. } => {
            assert_eq!(agent_message.as_deref(), Some("write_markdown 带说明"));
        }
        other => panic!("expected TrackReportEdited, got {other:?}"),
    }

    let err = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "kind": "prose", "markdown": "# X\n", "message": "  " }),
    )
    .await
    .expect_err("blank message");
    assert_eq!(err.code, -32602, "{err:?}");
}

#[tokio::test]
async fn commit_touching_a_task_block_illegally_is_refused_as_a_whole() {
    let boot = boot().await;
    let (task_id, _) = seed_planner_task(&boot, "batch-task").await;
    assert_eq!(task_keys(&boot).await, vec!["batch-task".to_string()]);
    let before = current_payload(&boot).await;
    let mut rx = boot.ctx.events.subscribe();

    let mut flipped = planner_task_payload("batch-task", "build it");
    flipped["declared_by"] = json!("user");
    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        commit_args(json!([
            { "op": "upsert", "kind": "prose", "markdown": "# 说明\n\nwould land\n" },
            { "op": "upsert", "id": task_id, "kind": "task", "payload": flipped }
        ])),
    )
    .await
    .expect_err("declared_by is immutable");
    assert_eq!(err.code, -32602, "{err:?}");
    assert!(err.message.contains("declared_by is immutable"), "{err:?}");

    let after = current_payload(&boot).await;
    assert_eq!(after.doc_rev, before.doc_rev);
    assert_eq!(after.body, before.body, "the prose op rolled back too");
    assert_eq!(task_keys(&boot).await, vec!["batch-task".to_string()]);
    assert!(drain_events(&mut rx).await.is_empty(), "nothing emitted");

    let seeded = planner_task_payload("batch-task", "build it");
    let tombstone = json!({
        "key": "batch-task", "tombstone": { "reason": "done with it" },
        "declared_by": seeded["declared_by"], "tombstoned_by": seeded["declared_by"]
    });
    let out = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        commit_args(json!([
            { "op": "upsert", "kind": "prose", "markdown": "# 说明\n\nlands\n" },
            { "op": "upsert", "id": task_id, "kind": "task", "payload": tombstone }
        ])),
    )
    .await
    .expect("legal tombstone in a batch");
    assert_eq!(out["docRev"].as_u64(), Some(before.doc_rev + 1));
}

/// #1883: a batch `delete` naming a live task by id retires it, alongside the batch's other ops.
#[tokio::test]
async fn commit_delete_naming_a_live_task_retires_it_inside_a_batch() {
    let boot = boot().await;
    let (task_id, _) = seed_planner_task(&boot, "batch-task").await;
    let before = current_payload(&boot).await;

    let out = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        commit_args(json!([
            { "op": "upsert", "kind": "prose", "markdown": "# Notes\n\nlands\n" },
            { "op": "delete", "id": task_id }
        ])),
    )
    .await
    .expect("a delete by id may retire a live task");
    assert_eq!(out["docRev"].as_u64(), Some(before.doc_rev + 1));
    let after = current_payload(&boot).await;
    assert!(after.body.contains("# Notes\n\nlands\n"));
    assert!(!after.body.contains("neige-block task"));
    assert!(task_keys(&boot).await.is_empty(), "task row is withdrawn");
}

#[tokio::test]
async fn commit_rejects_duplicate_block_ids_before_touching_the_doc() {
    let boot = boot().await;
    let index = seed_two_blocks(&boot).await;
    let (a_id, _) = index[0].clone();
    let (b_id, _) = index[1].clone();
    let before = current_payload(&boot).await;
    assert_eq!(before.doc_rev, 1);
    let mut rx = boot.ctx.events.subscribe();

    // A content-changing upsert would bump A to rev 2, so a later op on A could never match the read — refused up front instead of failing -32001.
    let cases: Vec<(&str, Value)> = vec![
        (
            "upsert then delete the same id",
            json!([
                { "op": "upsert", "id": a_id, "kind": "prose", "markdown": "# A\n\nv2\n" },
                { "op": "delete", "id": a_id }
            ]),
        ),
        (
            "move then move the same id",
            json!([
                { "op": "move", "id": b_id, "to_index": 0 },
                { "op": "upsert", "kind": "prose", "markdown": "# C\n\nnew\n" },
                { "op": "move", "id": b_id, "to_index": 2 }
            ]),
        ),
        (
            "delete then upsert the same id",
            json!([
                { "op": "delete", "id": b_id },
                { "op": "upsert", "id": b_id, "kind": "prose", "markdown": "# B\n\nback\n" }
            ]),
        ),
    ];
    for (name, ops) in cases {
        let err = call_tool(
            &boot,
            TOOL_REPORT_COMMIT,
            planner_identity(&boot),
            commit_args(ops),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("{name}: must be refused"));
        assert_eq!(err.code, -32602, "{name}: {err:?}");
        assert!(
            err.message
                .contains("each block id may appear at most once per commit"),
            "{name}: {err:?}"
        );
        assert!(
            err.message.contains("ops[1]:") || err.message.contains("ops[2]:"),
            "{name}: names the offending op: {err:?}"
        );
    }

    let after = current_payload(&boot).await;
    assert_eq!(after.doc_rev, before.doc_rev, "docRev unchanged");
    assert_eq!(after.body, before.body, "nothing persisted");
    assert_eq!(index_of(&read(&boot, json!({})).await), index);
    assert!(drain_events(&mut rx).await.is_empty(), "nothing emitted");

    // Two creates (no id) in one commit are fine — they address nothing.
    let out = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        commit_args(json!([
            { "op": "upsert", "kind": "prose", "markdown": "# C\n\nc\n" },
            { "op": "upsert", "kind": "prose", "markdown": "# D\n\nd\n" }
        ])),
    )
    .await
    .expect("two creates");
    assert_eq!(out["docRev"].as_u64(), Some(2));
}

/// Keys out of declaration order and an explicit `"omit_if_empty":false`, so a stored canonical line proves the ingress rewrote it.
const NON_CANONICAL_HEADER: &str = "<!-- neige:contract {\"sections\":[{\"omit_if_empty\":false,\"h1\":\"概要\"}],\"version\":1} -->";

fn one_section_header() -> calm_types::report_contract::ContractHeader {
    calm_types::report_contract::ContractHeader {
        version: 1,
        sections: vec![calm_types::report_contract::ContractSection {
            h1: "概要".into(),
            omit_if_empty: false,
        }],
    }
}

fn first_line(body: &str) -> &str {
    body.split('\n').next().unwrap_or_default()
}

/// Persisted `track.report_edited` rows — the audit-log view of "did a write land".
async fn report_edited_rows(boot: &Boot) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'track.report_edited'")
        .fetch_one(&boot.repo.sqlite_pool().expect("fixture repo is sqlite"))
        .await
        .expect("count persisted report edits")
}

/// The op itself is well-formed — it is the *document* the funnel refuses.
#[tokio::test]
async fn move_that_displaces_the_contract_block_is_rejected_by_the_funnel() {
    let boot = boot().await;
    let appended = upsert_block(
        &boot,
        planner_identity(&boot),
        json!({ "kind": "prose", "markdown": "# Appended\n\nlast\n"}),
    )
    .await
    .expect("an append keeps the header on line 1");
    let id = appended["id"].as_str().unwrap().to_string();
    let before = current_payload(&boot).await;
    assert_eq!(before.doc_rev, 1);
    assert!(
        first_line(&before.body).starts_with(calm_types::report_contract::HEADER_OPEN),
        "fixture: the birth body leads with the header"
    );
    let edits_before = report_edited_rows(&boot).await;
    let mut rx = boot.ctx.events.subscribe();

    let err = read_then_commit(
        &boot,
        planner_identity(&boot),
        json!([{"op": "move",  "id": id, "to_index": 0}]),
    )
    .await
    .expect_err("moving a block above the contract block displaces the header");
    assert_eq!(err.code, RpcError::INVALID_PARAMS, "{err:?}");
    assert!(
        err.message.contains("first line"),
        "the header's own Misplaced message: {err:?}"
    );

    assert_eq!(
        current_payload(&boot).await,
        before,
        "the tx aborted: docRev, body and blocks are what they were"
    );
    assert_eq!(
        report_edited_rows(&boot).await,
        edits_before,
        "no track.report_edited row"
    );
    assert!(drain_events(&mut rx).await.is_empty(), "nothing broadcast");

    read_then_commit(
        &boot,
        planner_identity(&boot),
        json!([{"op": "move",  "id": id, "to_index": 1}]),
    )
    .await
    .expect("moving below the contract block is an ordinary reorder");
    assert_eq!(current_payload(&boot).await.doc_rev, 2);
}

#[tokio::test]
async fn upsert_prose_at_position_0_with_a_header_is_accepted_only_when_the_doc_has_none() {
    use calm_types::report_contract::{canonical_line, check_document};

    let headless = boot().await;
    let _ = seed_two_blocks(&headless).await; // docRev 1, no header
    upsert_block(
        &headless,
        planner_identity(&headless),
        json!({
            "kind": "prose",
            "markdown": format!("{NON_CANONICAL_HEADER}\n"),
            "position": 0
        }),
    )
    .await
    .expect("the first header, on line 1");
    let payload = current_payload(&headless).await;
    assert_eq!(
        first_line(&payload.body),
        canonical_line(&one_section_header()),
        "stored canonical, not as sent: {:?}",
        payload.body
    );
    assert_eq!(
        check_document(&payload.body),
        Ok(Some(one_section_header()))
    );

    let birth = boot().await; // birth body: header already on line 1
    let before = current_payload(&birth).await;
    let edits_before = report_edited_rows(&birth).await;
    let err = upsert_block(
        &birth,
        planner_identity(&birth),
        json!({
            "kind": "prose",
            "markdown": format!("{NON_CANONICAL_HEADER}\n"),
            "position": 0
        }),
    )
    .await
    .expect_err("a second header");
    assert_eq!(err.code, RpcError::INVALID_PARAMS, "{err:?}");
    assert!(
        err.message.contains("at most one contract header"),
        "the header's own Duplicate message: {err:?}"
    );
    assert_eq!(current_payload(&birth).await, before, "nothing landed");
    assert_eq!(report_edited_rows(&birth).await, edits_before);
}

/// The funnel judges the document a batch leaves behind, not its steps.
#[tokio::test]
async fn commit_whose_steps_leave_the_header_off_line_1_is_rejected_as_a_whole() {
    use calm_types::report_contract::canonical_line;

    let boot = boot().await;
    let index = seed_two_blocks(&boot).await; // docRev 1, A@0, B@1, no header
    let (a_id, _) = index[0].clone();
    let before = current_payload(&boot).await;
    let edits_before = report_edited_rows(&boot).await;
    let mut rx = boot.ctx.events.subscribe();

    let err = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        commit_args(json!([
                { "op": "upsert", "kind": "prose", "markdown": format!("{NON_CANONICAL_HEADER}\n"), "position": 0 },
                { "op": "move", "id": a_id, "to_index": 0 }
            ]),
        ),
    )
    .await
    .expect_err("the move puts A above the header block");
    assert_eq!(err.code, RpcError::INVALID_PARAMS, "{err:?}");
    assert!(err.message.contains("first line"), "{err:?}");
    assert!(
        !err.message.contains("ops["),
        "refused by the funnel on the whole document, not by a step: {err:?}"
    );
    assert_eq!(current_payload(&boot).await, before, "nothing landed");
    assert_eq!(report_edited_rows(&boot).await, edits_before);
    assert!(drain_events(&mut rx).await.is_empty(), "nothing emitted");

    let out = call_tool(
        &boot,
        TOOL_REPORT_COMMIT,
        planner_identity(&boot),
        commit_args(json!([
                { "op": "upsert", "kind": "prose", "markdown": format!("{NON_CANONICAL_HEADER}\n"), "position": 0 }
            ]),
        ),
    )
    .await
    .expect("the upsert alone lands the header on line 1");
    assert_eq!(out["docRev"], 2);
    assert_eq!(
        first_line(&current_payload(&boot).await.body),
        canonical_line(&one_section_header())
    );
}
