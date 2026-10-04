//! First-class views through production report write/read handlers; no plugin is started.
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
    call(boot, "neige.report.read", args).await
}

fn reference() -> Value {
    json!({"source": "neige://plugin/operations/health", "version": 1})
}

fn presentation() -> Value {
    serde_json::from_str::<Value>(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-data/native-view-v1.json"
    )))
    .unwrap()["valid"]
        .clone()
}

fn records_presentation(count: usize, body: String, sections: usize) -> Value {
    let mut data = presentation();
    let records = &mut data["rows"][2]["cells"][0]["datasets"][0]["items"];
    let item = records[0].clone();
    *records = Value::Array(
        (0..count)
            .map(|index| {
                let mut item = item.clone();
                item["id"] = json!(format!("record-{index}"));
                item["summary"] = json!(body);
                item["sections"] = Value::Array(
                    (0..sections)
                        .map(|_| json!({"label":"Full text", "body":body}))
                        .collect(),
                );
                item
            })
            .collect(),
    );
    data
}

#[tokio::test]
async fn live_view_preserves_full_records_above_the_persisted_write_budget() {
    let boot = boot().await;
    let created = create(&boot, "view.live", reference()).await;
    let data = records_presentation(50, "x".repeat(6000), 0);
    assert!(calm_types::report_blocks::native_view::validate(&data).is_ok());
    assert!(calm_types::report_blocks::validate_payload("view", &data).is_err());
    assert!(serde_json::to_vec(&data).unwrap().len() < 4 * 1024 * 1024);
    publish(
        &boot,
        "operations",
        "track",
        boot.track_id.as_str(),
        "health",
        data.clone(),
    )
    .await;
    let report = read(
        &boot,
        json!({"resolve": {created["id"].as_str().unwrap(): "full"}}),
    )
    .await;
    assert_eq!(resolved(&report, &created["id"])["status"], "ok");
    assert_eq!(resolved(&report, &created["id"])["data"], data);
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
async fn live_view_storage_error_is_not_reported_as_pending() {
    let boot = boot().await;
    let created = create(&boot, "view.live", reference()).await;
    // Corrupt only this test's in-memory repository, after the report exists.
    sqlx::query("DROP TABLE overlays")
        .execute(&boot.repo.sqlite_pool().unwrap())
        .await
        .unwrap();
    let out = read(&boot, json!({})).await;
    assert_eq!(resolved(&out, &created["id"])["status"], "unavailable");
    assert_eq!(
        resolved(&out, &created["id"])["reason"],
        "overlay storage unavailable"
    );
    let none = read(
        &boot,
        json!({"resolve": {created["id"].as_str().unwrap(): "none"}}),
    )
    .await;
    assert!(resolved(&none, &created["id"]).is_null());
    assert_eq!(none["docRev"], out["docRev"]);
}

#[tokio::test]
async fn live_view_discovery_write_read_and_stale_cas() {
    let boot = boot().await;
    let kinds = call(&boot, "neige.report.kinds", json!({})).await;
    let kind = kinds["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["kind"] == "view.live")
        .unwrap();
    assert_eq!(kind["schema"]["required"], json!(["source", "version"]));
    let created = create(&boot, "view.live", reference()).await;
    let before = read(&boot, json!({})).await;
    assert!(
        before["text"]
            .as_str()
            .unwrap()
            .contains("```neige-block view.live\n")
    );
    assert_eq!(resolved(&before, &created["id"])["status"], "pending");
    let mut changed = reference();
    changed["source"] = json!("neige://plugin/operations/updated-health");
    let updated = upsert_block(
        &boot,
        assistant_identity(&boot),
        json!({"id":created["id"], "kind":"view.live", "payload":changed}),
    )
    .await
    .unwrap();
    let error = call_tool(
        &boot,
        "neige.report.commit",
        planner_identity(&boot),
        json!({
            "message": "stale replacement",
            "ops": [{"op":"upsert", "id": created["id"], "kind": "view.live", "payload": reference()}],
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32001);
    assert_eq!(read(&boot, json!({})).await["docRev"], updated["docRev"]);
    let commit = call(
        &boot,
        "neige.report.commit",
        json!({"message": "append view",
        "ops": [{"op": "upsert", "kind": "view.live", "payload": reference()}]}),
    )
    .await;
    assert!(commit["docRev"].as_u64().unwrap() > before["docRev"].as_u64().unwrap());
    let snapshot = read(&boot, json!({})).await;
    call(
        &boot,
        "neige.report.write",
        json!({"body": snapshot["text"], "message": "round trip"}),
    )
    .await;
    assert_eq!(read(&boot, json!({})).await["text"], snapshot["text"]);
}

#[tokio::test]
async fn live_view_hydration_is_scoped_bounded_and_read_only() {
    let boot = boot().await;
    let created = create(&boot, "view.live", reference()).await;
    let id = &created["id"];
    let track = boot.track_id.as_str();
    let data = presentation();
    for (plugin, entity_kind, entity_id, kind) in [
        ("other", "track", track, "health"),
        ("operations", "area", track, "health"),
        ("operations", "track", "other-track", "health"),
        ("operations", "track", track, "other"),
    ] {
        publish(&boot, plugin, entity_kind, entity_id, kind, data.clone()).await;
    }
    assert_eq!(
        resolved(&read(&boot, json!({})).await, id)["status"],
        "pending"
    );
    publish(&boot, "operations", "track", track, "health", data.clone()).await;
    let before = read(&boot, json!({})).await;
    let summary = resolved(&before, id);
    assert_eq!(summary["status"], "ok");
    assert_eq!(summary["validation"], "presentation");
    assert_eq!(summary["source"], reference()["source"]);
    assert_eq!(summary["version"], 1);
    assert!(summary["resolved_at"].is_string());
    assert!(summary.get("data").is_none());
    let full = read(&boot, json!({"resolve": {id.as_str().unwrap(): "full"}})).await;
    assert_eq!(resolved(&full, id)["data"], data);
    assert_eq!(full["docRev"], before["docRev"]);
    let none = read(&boot, json!({"resolve": {id.as_str().unwrap(): "none"}})).await;
    assert!(resolved(&none, id).is_null());
    let mut wrong_version = data.clone();
    wrong_version["version"] = json!(2);
    let mut wrong_shape = data.clone();
    wrong_shape["rows"][0]["cells"][0]["action"] = json!("run");
    let oversized = records_presentation(50, "😀".repeat(8000), 2);
    assert!(calm_types::report_blocks::native_view::validate(&oversized).is_ok());
    assert!(serde_json::to_vec(&oversized).unwrap().len() > 4 * 1024 * 1024);
    for data in [wrong_version, wrong_shape, oversized] {
        publish(&boot, "operations", "track", track, "health", data).await;
        let out = read(&boot, json!({"resolve": {id.as_str().unwrap(): "full"}})).await;
        assert_eq!(resolved(&out, id)["status"], "unavailable");
        assert!(resolved(&out, id).get("data").is_none());
        assert_eq!(out["docRev"], before["docRev"]);
    }
}

#[tokio::test]
async fn live_view_does_not_expand_the_table_contract() {
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
async fn live_view_table_hydration_preserves_large_existing_overlay() {
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
async fn live_view_table_hydration_preserves_nullable_overlay_fields() {
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

#[tokio::test]
async fn live_view_invalid_reference_cannot_mutate_report() {
    let boot = boot().await;
    let before = read(&boot, json!({})).await;
    for bad in [
        json!({"source": "https://example.com", "version": 1}),
        json!({"source": "neige://plugin/operations/health", "version": 1, "view": "html"}),
    ] {
        for (tool, args) in [
            (
                "neige.report.commit",
                json!({"message": "bad", "ops": [{"op": "upsert", "kind": "view.live", "payload": bad}]}),
            ),
            (
                "neige.report.write",
                json!({"message": "bad", "body": format!("```neige-block view.live\n{bad}\n```\n")}),
            ),
        ] {
            let error = call_tool(&boot, tool, planner_identity(&boot), args)
                .await
                .unwrap_err();
            assert_eq!(error.code, -32602, "{tool}: {error:?}");
            assert_eq!(read(&boot, json!({})).await["docRev"], before["docRev"]);
        }
    }
}
