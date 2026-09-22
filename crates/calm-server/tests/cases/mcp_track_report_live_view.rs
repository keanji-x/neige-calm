//! First-class views through production report write/read handlers; no plugin is started.
#![cfg(unix)]

use crate::mcp_track_report::{Boot, boot, call_tool, planner_identity};
use calm_server::model::NewOverlay;
use serde_json::{Value, json};

async fn call(boot: &Boot, tool: &str, args: Value) -> Value {
    call_tool(boot, tool, planner_identity(boot), args)
        .await
        .unwrap()
}

async fn read(boot: &Boot, args: Value) -> Value {
    call(boot, "calm.report.read", args).await
}

fn reference() -> Value {
    json!({"source": "neige://plugin/operations/health", "version": 1, "view": "overview"})
}

async fn create(boot: &Boot, kind: &str, payload: Value) -> Value {
    let rev = read(boot, json!({})).await["docRev"].clone();
    call(
        boot,
        "calm.report.blocks.upsert",
        json!({"kind": kind, "payload": payload, "if_doc_rev": rev}),
    )
    .await
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
    let kinds = call(&boot, "calm.report.blocks.kinds", json!({})).await;
    let kind = kinds["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["kind"] == "view.live")
        .unwrap();
    assert_eq!(
        kind["schema"]["required"],
        json!(["source", "version", "view"])
    );
    let created = create(&boot, "view.live", reference()).await;
    let before = read(&boot, json!({})).await;
    assert!(
        before["text"]
            .as_str()
            .unwrap()
            .contains("```neige-block view.live\n")
    );
    assert_eq!(resolved(&before, &created["id"])["status"], "pending");
    let error = call_tool(
        &boot,
        "calm.report.blocks.upsert",
        planner_identity(&boot),
        json!({
            "id": created["id"], "kind": "view.live", "payload": reference(), "if_rev": 999,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32001);
    assert_eq!(read(&boot, json!({})).await["docRev"], before["docRev"]);
    let commit = call(
        &boot,
        "calm.report.commit",
        json!({"if_doc_rev": before["docRev"], "message": "append view",
        "ops": [{"op": "upsert", "kind": "view.live", "payload": reference()}]}),
    )
    .await;
    assert!(commit["docRev"].as_u64().unwrap() > before["docRev"].as_u64().unwrap());
    let snapshot = read(&boot, json!({})).await;
    call(&boot, "calm.report.write_markdown", json!({"body": snapshot["text"], "if_doc_rev": snapshot["docRev"], "message": "round trip"})).await;
    assert_eq!(read(&boot, json!({})).await["text"], snapshot["text"]);
}

#[tokio::test]
async fn live_view_hydration_is_scoped_bounded_and_read_only() {
    let boot = boot().await;
    let created = create(&boot, "view.live", reference()).await;
    let id = &created["id"];
    let track = boot.track_id.as_str();
    let data = json!({"version": 1, "view": "overview", "metrics": [], "notices": [], "charts": [], "updated": null});
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
    assert_eq!(summary["validation"], "envelope-only");
    assert_eq!(summary["source"], reference()["source"]);
    assert_eq!(summary["version"], 1);
    assert_eq!(summary["view"], "overview");
    assert!(summary["resolved_at"].is_string());
    assert!(summary.get("data").is_none());
    let full = read(&boot, json!({"resolve": {id.as_str().unwrap(): "full"}})).await;
    assert_eq!(resolved(&full, id)["data"], data);
    assert_eq!(full["docRev"], before["docRev"]);
    let none = read(&boot, json!({"resolve": {id.as_str().unwrap(): "none"}})).await;
    assert!(resolved(&none, id).is_null());
    for data in [
        json!({"version": 2, "view": "overview"}),
        json!({"version": 1, "view": "cards"}),
        json!({"version": 1, "view": "overview", "text": "x".repeat(4 * 1024 * 1024)}),
    ] {
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
        json!({"source": "https://example.com", "version": 1, "view": "overview"}),
        json!({"source": "neige://plugin/operations/health", "version": 1, "view": "html"}),
    ] {
        for (tool, args) in [
            (
                "calm.report.blocks.upsert",
                json!({"kind": "view.live", "payload": bad, "if_doc_rev": before["docRev"]}),
            ),
            (
                "calm.report.commit",
                json!({"if_doc_rev": before["docRev"], "message": "bad", "ops": [{"op": "upsert", "kind": "view.live", "payload": bad}]}),
            ),
            (
                "calm.report.write_markdown",
                json!({"if_doc_rev": before["docRev"], "message": "bad", "body": format!("```neige-block view.live\n{bad}\n```\n")}),
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
