//! Live tables and template views with live slots through production report write/read handlers;
//! no plugin is started.
#![cfg(unix)]

use crate::mcp_track_report::{
    Boot, assistant_identity, boot, call_tool, planner_identity, upsert_block,
};
use calm_server::model::NewOverlay;
use serde_json::{Value, json};

async fn call(boot: &Boot, tool: &str, args: Value) -> Value {
    call_tool(boot, tool, planner_identity(boot), args)
        .await
        .unwrap()
}

async fn read(boot: &Boot, args: Value) -> Value {
    call(boot, "neige_report_read", args).await
}

fn presentation() -> Value {
    serde_json::from_str::<Value>(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-data/native-view-v1.json"
    )))
    .unwrap()["valid"]
        .clone()
}

async fn create(boot: &Boot, kind: &str, payload: Value) -> Value {
    upsert_block(
        boot,
        planner_identity(boot),
        json!({"kind": kind, "payload": payload}),
    )
    .await
    .unwrap()
}

async fn publish(
    boot: &Boot,
    plugin: &str,
    entity_kind: &str,
    track: &str,
    kind: &str,
    payload: Value,
) {
    boot.repo
        .overlay_upsert(NewOverlay {
            plugin_id: plugin.into(),
            entity_kind: entity_kind.into(),
            entity_id: track.into(),
            kind: kind.into(),
            payload,
        })
        .await
        .unwrap();
}

fn resolved<'a>(report: &'a Value, id: &Value) -> &'a Value {
    &report["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| &block["id"] == id)
        .unwrap()["resolved"]
}

#[tokio::test]
async fn live_table_does_not_expand_the_table_contract() {
    let boot = boot().await;
    let created = create(
        &boot,
        "table",
        json!({"source": "neige://plugin/operations/health"}),
    )
    .await;
    let table = json!({"columns": [{"key": "a", "label": "A"}], "rows": [{"a": 1}]});
    let mut mixed = table.clone();
    mixed["view"] = json!("overview");
    for data in [
        mixed,
        json!({"source": "neige://plugin/x/y"}),
        json!({"columns": [{"key": "a", "label": "A"}], "rows": [{"b": 1}]}),
        json!({"columns": [], "rows": []}),
    ] {
        publish(
            &boot,
            "operations",
            "track",
            boot.track_id.as_str(),
            "health",
            data,
        )
        .await;
        assert_eq!(
            resolved(&read(&boot, json!({})).await, &created["id"])["status"],
            "unavailable"
        );
    }
    publish(
        &boot,
        "operations",
        "track",
        boot.track_id.as_str(),
        "health",
        table.clone(),
    )
    .await;
    let full = read(
        &boot,
        json!({"resolve": {created["id"].as_str().unwrap(): "full"}}),
    )
    .await;
    assert_eq!(resolved(&full, &created["id"])["table"], table);
}

#[tokio::test]
async fn live_table_hydration_preserves_large_existing_overlay() {
    let boot = boot().await;
    let created = create(
        &boot,
        "table",
        json!({"source": "neige://plugin/operations/health"}),
    )
    .await;
    let table = json!({"columns": [{"key": "a", "label": "A"}, {"key": "b", "label": "B"}],
        "rows": vec![json!({"a": "x".repeat(2048), "b": "y".repeat(2048)}); 65]});
    assert!(serde_json::to_vec(&table).unwrap().len() > 256 * 1024);
    publish(
        &boot,
        "operations",
        "track",
        boot.track_id.as_str(),
        "health",
        table.clone(),
    )
    .await;
    let full = read(
        &boot,
        json!({"resolve": {created["id"].as_str().unwrap(): "full"}}),
    )
    .await;
    assert_eq!(resolved(&full, &created["id"])["status"], "ok");
    assert_eq!(resolved(&full, &created["id"])["table"], table);
}

#[tokio::test]
async fn live_table_hydration_preserves_nullable_overlay_fields() {
    let boot = boot().await;
    let created = create(
        &boot,
        "table",
        json!({"source": "neige://plugin/operations/health"}),
    )
    .await;
    let table = json!({"columns": [{"key": "a", "label": "A", "align": null}],
        "rows": [{"a": 1}], "caption": null, "highlight": null});
    publish(
        &boot,
        "operations",
        "track",
        boot.track_id.as_str(),
        "health",
        table.clone(),
    )
    .await;
    let full = read(
        &boot,
        json!({"resolve": {created["id"].as_str().unwrap(): "full"}}),
    )
    .await;
    assert_eq!(resolved(&full, &created["id"])["status"], "ok");
    assert_eq!(resolved(&full, &created["id"])["table"], table);
}

// Template views with live slots (#2021 S2): each slot resolves through the same exact lookup.

fn slot(id: &str, kind: &str, expects: &str) -> Value {
    json!({"kind": "live", "id": id, "expects": expects,
           "source": format!("neige://plugin/operations/{kind}")})
}

fn template(rows: Vec<Vec<Value>>) -> Value {
    let rows: Vec<Value> = rows
        .into_iter()
        .enumerate()
        .map(|(index, cells)| {
            let layout = ["one", "two", "three"][cells.len() - 1];
            json!({"id": format!("row-{index}"), "title": "", "layout": layout, "cells": cells})
        })
        .collect();
    json!({"version": 1, "title": "", "description": "", "snapshot": null, "rows": rows})
}

fn unit(snapshot: &str, cell: Value) -> Value {
    json!({"snapshot": {"id": snapshot, "observedAt": 1790035200000_u64, "producedAt": null},
           "cell": cell})
}

/// A cell of the shared valid fixture: (0,0) metrics, (0,1) time-series, (1,1) table.
fn component(row: usize, cell: usize) -> Value {
    presentation()["rows"][row]["cells"][cell].clone()
}

fn table_unit(rows: usize) -> Value {
    let columns: Vec<Value> = (0..5)
        .map(|i| json!({"key": format!("c{i}"), "label": ""}))
        .collect();
    let row: serde_json::Map<String, Value> = (0..5)
        .map(|i| (format!("c{i}"), Value::String("x".repeat(1800))))
        .collect();
    unit(
        "detail-r1",
        json!({"kind": "table", "id": "big", "title": "",
               "table": {"columns": columns, "rows": vec![Value::Object(row); rows]}}),
    )
}

async fn publish_unit(boot: &Boot, plugin: &str, kind: &str, payload: Value) {
    publish(boot, plugin, "track", boot.track_id.as_str(), kind, payload).await;
}

async fn read_full(boot: &Boot, id: &Value) -> Value {
    read(boot, json!({"resolve": {id.as_str().unwrap(): "full"}})).await
}

fn cells(resolved: &Value) -> &Vec<Value> {
    resolved["cells"].as_array().unwrap()
}

#[tokio::test]
async fn live_slots_resolve_their_own_units() {
    let boot = boot().await;
    let inline = create(&boot, "view", presentation()).await;
    let created = create(
        &boot,
        "view",
        template(vec![vec![
            slot("nav", "capacity.summary", "metrics"),
            slot("history", "capacity.history", "time-series"),
        ]]),
    )
    .await;
    let id = &created["id"];
    let summary_unit = unit("summary-r1", component(0, 0));
    let history_unit = unit("history-r1", component(0, 1));
    publish_unit(
        &boot,
        "operations",
        "capacity.summary",
        summary_unit.clone(),
    )
    .await;
    publish_unit(
        &boot,
        "operations",
        "capacity.history",
        history_unit.clone(),
    )
    .await;

    let summary = read(&boot, json!({})).await;
    assert!(
        resolved(&summary, &inline["id"]).is_null(),
        "an inline-only view has no slots"
    );
    let view = resolved(&summary, id);
    assert_eq!(view["status"], "ok");
    assert_eq!(view["validation"], "presentation");
    let ids: Vec<&Value> = cells(view).iter().map(|cell| &cell["id"]).collect();
    assert_eq!(ids, [&json!("nav"), &json!("history")]);
    for (cell, kind) in cells(view)
        .iter()
        .zip(["capacity.summary", "capacity.history"])
    {
        assert_eq!(cell["status"], "ok", "{cell}");
        assert_eq!(cell["source"], format!("neige://plugin/operations/{kind}"));
        assert_eq!(cell["observed_at"], "2026-09-22T00:00:00Z");
        assert!(cell["resolved_at"].is_string());
        assert!(cell.get("data").is_none());
    }

    let full = read_full(&boot, id).await;
    assert_eq!(cells(resolved(&full, id))[0]["data"], summary_unit);
    assert_eq!(cells(resolved(&full, id))[1]["data"], history_unit);
    assert_eq!(full["docRev"], summary["docRev"]);
}

#[tokio::test]
async fn live_slot_ignores_another_plugins_overlay_of_the_same_kind() {
    let boot = boot().await;
    let created = create(
        &boot,
        "view",
        template(vec![vec![slot("nav", "capacity.summary", "metrics")]]),
    )
    .await;
    let id = &created["id"];
    let track = boot.track_id.as_str();
    let foreign = unit("foreign-r1", component(0, 0));
    for (plugin, entity_kind, entity_id, kind) in [
        ("other", "track", track, "capacity.summary"),
        ("operations", "area", track, "capacity.summary"),
        ("operations", "track", "other-track", "capacity.summary"),
        ("operations", "track", track, "capacity.other"),
    ] {
        publish(&boot, plugin, entity_kind, entity_id, kind, foreign.clone()).await;
    }
    let view = read_full(&boot, id).await;
    assert_eq!(resolved(&view, id)["status"], "partial");
    assert_eq!(cells(resolved(&view, id))[0]["status"], "pending");
    assert!(cells(resolved(&view, id))[0].get("data").is_none());

    let own = unit("own-r1", component(0, 0));
    publish_unit(&boot, "operations", "capacity.summary", own.clone()).await;
    let view = read_full(&boot, id).await;
    assert_eq!(resolved(&view, id)["status"], "ok");
    assert_eq!(cells(resolved(&view, id))[0]["data"], own);
}

#[tokio::test]
async fn live_slot_pending_is_not_storage_unavailable() {
    let boot = boot().await;
    let created = create(
        &boot,
        "view",
        template(vec![vec![slot("nav", "capacity.summary", "metrics")]]),
    )
    .await;
    let id = &created["id"];
    let pending = read(&boot, json!({})).await;
    let view = resolved(&pending, id);
    assert_eq!(view["status"], "partial");
    assert_eq!(cells(view).len(), 1);
    assert_eq!(cells(view)[0]["status"], "pending");
    assert!(cells(view)[0].get("reason").is_none());
    assert!(cells(view)[0].get("resolved_at").is_none());

    // Corrupt only this test's in-memory repository, after the report exists.
    sqlx::query("DROP TABLE overlays")
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let broken = read(&boot, json!({})).await;
    assert_eq!(
        resolved(&broken, id),
        &json!({"status": "unavailable", "reason": "overlay storage unavailable"})
    );
    assert_eq!(broken["docRev"], pending["docRev"]);
}

#[tokio::test]
async fn live_slot_none_resolution_reads_no_overlay() {
    let boot = boot().await;
    let created = create(
        &boot,
        "view",
        template(vec![vec![slot("nav", "capacity.summary", "metrics")]]),
    )
    .await;
    let id = &created["id"];
    publish_unit(
        &boot,
        "operations",
        "capacity.summary",
        unit("r1", component(0, 0)),
    )
    .await;
    let before = read(&boot, json!({})).await;
    assert_eq!(resolved(&before, id)["status"], "ok");
    sqlx::query("DROP TABLE overlays")
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let none = read(&boot, json!({"resolve": {id.as_str().unwrap(): "none"}})).await;
    assert!(resolved(&none, id).is_null());
    assert_eq!(none["docRev"], before["docRev"]);
}

#[tokio::test]
async fn live_slot_unit_is_capped_per_unit() {
    let boot = boot().await;
    let created = create(
        &boot,
        "view",
        template(vec![vec![slot("detail", "capacity.detail", "table")]]),
    )
    .await;
    let id = &created["id"];
    let oversized = table_unit(500);
    assert!(serde_json::to_vec(&oversized).unwrap().len() > 4 * 1024 * 1024);
    publish_unit(&boot, "operations", "capacity.detail", oversized).await;
    let out = read_full(&boot, id).await;
    let cell = &cells(resolved(&out, id))[0];
    assert_eq!(resolved(&out, id)["status"], "partial");
    assert_eq!(cell["status"], "unavailable");
    assert!(
        cell["reason"].as_str().unwrap().contains("byte limit"),
        "{cell}"
    );
    assert!(cell["resolved_at"].is_string());
    assert!(cell.get("data").is_none());

    // Under the unit cap but far above the 256 KiB persisted write budget.
    let large = table_unit(400);
    assert!(serde_json::to_vec(&large).unwrap().len() > 256 * 1024);
    publish_unit(&boot, "operations", "capacity.detail", large.clone()).await;
    let out = read_full(&boot, id).await;
    assert_eq!(resolved(&out, id)["status"], "ok");
    assert_eq!(cells(resolved(&out, id))[0]["data"], large);
}

#[tokio::test]
async fn one_bad_slot_leaves_the_others_ok() {
    let boot = boot().await;
    let created = create(
        &boot,
        "view",
        template(vec![vec![
            slot("nav", "capacity.summary", "metrics"),
            slot("history", "capacity.history", "time-series"),
            slot("detail", "capacity.detail", "table"),
        ]]),
    )
    .await;
    let id = &created["id"];
    let good = unit("summary-r1", component(0, 0));
    let mut malformed = unit("detail-r1", component(1, 1));
    malformed["cell"]["action"] = json!("run");
    publish_unit(&boot, "operations", "capacity.summary", good.clone()).await;
    publish_unit(&boot, "operations", "capacity.detail", malformed).await;

    let out = read_full(&boot, id).await;
    let view = resolved(&out, id);
    assert_eq!(view["status"], "partial");
    let [nav, history, detail] = cells(view).as_slice() else {
        panic!("three slots: {view}");
    };
    assert_eq!((&nav["id"], &nav["status"]), (&json!("nav"), &json!("ok")));
    assert_eq!(nav["data"], good);
    assert_eq!(
        (&history["id"], &history["status"]),
        (&json!("history"), &json!("pending"))
    );
    assert!(history.get("data").is_none());
    assert_eq!(
        (&detail["id"], &detail["status"]),
        (&json!("detail"), &json!("unavailable"))
    );
    assert!(
        detail["reason"].as_str().unwrap().contains("action"),
        "{detail}"
    );
    assert!(detail.get("data").is_none());
}

#[tokio::test]
async fn live_slot_template_write_read_and_stale_cas() {
    let boot = boot().await;
    let original = template(vec![vec![slot("nav", "capacity.summary", "metrics")]]);
    let created = create(&boot, "view", original.clone()).await;
    let before = read(&boot, json!({})).await;
    assert!(
        before["text"]
            .as_str()
            .unwrap()
            .contains("```neige-block view\n")
    );
    let mut changed = original.clone();
    changed["rows"][0]["cells"][0] = slot("nav", "capacity.updated", "metrics");
    let updated = upsert_block(
        &boot,
        assistant_identity(&boot),
        json!({"id": created["id"], "kind": "view", "payload": changed}),
    )
    .await
    .unwrap();
    let error = call_tool(
        &boot,
        "neige_report_commit",
        planner_identity(&boot),
        json!({
            "message": "stale replacement",
            "ops": [{"op": "upsert", "id": created["id"], "kind": "view", "payload": original}],
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32001);
    let after = read_full(&boot, &created["id"]).await;
    assert_eq!(after["docRev"], updated["docRev"]);
    assert_eq!(
        cells(resolved(&after, &created["id"]))[0]["source"],
        "neige://plugin/operations/capacity.updated"
    );
    call(
        &boot,
        "neige_report_write",
        json!({"body": after["text"], "message": "round trip"}),
    )
    .await;
    assert_eq!(read(&boot, json!({})).await["text"], after["text"]);
}
