//! Plugin standing Planner instructions (#2104 K2) through the Planner's real `thread/start`
//! instructions: plugin rows in the store, report blocks written by the real commit tool, and a
//! plugin host that has loaded and spawned nothing.

#![cfg(unix)]

use std::sync::Arc;

use calm_server::model::NewPlugin;
use calm_server::operation::planner_harness_start_adapter::planner_instructions_for_test;
use calm_server::plugin_host::manifest::ManifestError;
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry};
use calm_types::report_blocks::{KIND_CHART_SERIES, KIND_TABLE, KIND_VIEW};
use serde_json::{Value, json};

use crate::mcp_track_report::{Boot, boot, planner_identity, upsert_block};

const NOTICE: &str = "## Plugin instructions omitted (over budget); see the server log\n";

fn manifest(version: u32, id: &str, text: &str) -> Value {
    json!({
        "manifest_version": version,
        "id": id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "K2 probe",
        "entrypoint": { "command": "bin/stub" },
        "planner_instructions": text,
    })
}

async fn install_row(boot: &Boot, id: &str, text: &str, enabled: bool) {
    boot.repo
        .plugin_install(NewPlugin {
            id: id.into(),
            version: "0.1.0".into(),
            install_path: format!("/nonexistent/{id}"),
            manifest: manifest(5, id, text),
            enabled,
            user_config: json!({}),
        })
        .await
        .expect("store the plugin row");
}

/// A host that has loaded no manifest from disk and spawned nothing.
fn host_before_boot(boot: &Boot) -> PluginHost {
    let dir = boot.ctx.gate_logs_dir.join(boot.track_id.as_str());
    PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        boot.repo.clone(),
        dir.join("plugins"),
        dir.join("plugin-data"),
        Vec::new(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
    )
}

fn source(id: &str) -> String {
    format!("neige://plugin/{id}/unit")
}

async fn write(boot: &Boot, kind: &str, payload: Value) {
    upsert_block(
        boot,
        planner_identity(boot),
        json!({ "kind": kind, "payload": payload }),
    )
    .await
    .expect("commit the block");
}

async fn reference_by_table(boot: &Boot, id: &str) {
    write(boot, KIND_TABLE, json!({ "source": source(id) })).await;
}

async fn reference_by_view(boot: &Boot, id: &str) {
    let slot = json!({ "kind": "live", "id": "slot", "expects": "metrics", "source": source(id) });
    let row = json!({ "id": "row-0", "title": "", "layout": "one", "cells": [slot] });
    let view =
        json!({ "version": 1, "title": "", "description": "", "snapshot": null, "rows": [row] });
    write(boot, KIND_VIEW, view).await;
}

async fn reference_by_series(boot: &Boot, id: &str) {
    write(
        boot,
        KIND_CHART_SERIES,
        json!({ "source": source(id), "series": ["US:NVDA"] }),
    )
    .await;
}

async fn prompt(boot: &Boot, host: &PluginHost) -> String {
    planner_instructions_for_test(
        boot.repo.as_ref(),
        host,
        boot.track_id.as_str(),
        boot.planner_card_id.as_str(),
    )
    .await
    .expect("render the Planner instructions")
}

fn block(id: &str, text: &str) -> String {
    format!("## Plugin {id}\n{text}\n")
}

#[tokio::test]
async fn plugin_instructions_follow_report_references() {
    let boot = boot().await;
    let host = host_before_boot(&boot);
    for (id, text) in [
        ("k2-series", "Series plugin method."),
        ("k2-table", "Table plugin method."),
        ("k2-view", "View plugin method."),
    ] {
        install_row(&boot, id, text, true).await;
    }
    reference_by_view(&boot, "k2-view").await;
    reference_by_table(&boot, "k2-table").await;
    reference_by_series(&boot, "k2-series").await;

    let prompt = prompt(&boot, &host).await;
    let expected = [
        block("k2-series", "Series plugin method."),
        block("k2-table", "Table plugin method."),
        block("k2-view", "View plugin method."),
    ]
    .concat();
    assert!(
        prompt.ends_with(&format!("\n\n{expected}")),
        "every referenced plugin instructs the Planner, in id order:\n{prompt}"
    );
}

#[tokio::test]
async fn plugin_instructions_skip_unreferenced_tracks() {
    let boot = boot().await;
    let host = host_before_boot(&boot);
    install_row(&boot, "k2-referenced", "Referenced method.", true).await;
    install_row(&boot, "k2-unreferenced", "Unreferenced method.", true).await;
    reference_by_table(&boot, "k2-referenced").await;

    let prompt = prompt(&boot, &host).await;
    assert!(
        prompt.contains(&block("k2-referenced", "Referenced method.")),
        "{prompt}"
    );
    assert!(
        !prompt.contains("Unreferenced method."),
        "a plugin this Track's report does not reference never instructs its Planner:\n{prompt}"
    );
}

#[tokio::test]
async fn plugin_instructions_skip_disabled_plugin() {
    let boot = boot().await;
    let host = host_before_boot(&boot);
    install_row(&boot, "k2-enabled", "Enabled method.", true).await;
    install_row(&boot, "k2-disabled", "Disabled method.", false).await;
    reference_by_table(&boot, "k2-enabled").await;
    reference_by_table(&boot, "k2-disabled").await;

    let prompt = prompt(&boot, &host).await;
    assert!(
        prompt.contains(&block("k2-enabled", "Enabled method.")),
        "{prompt}"
    );
    assert!(
        !prompt.contains("Disabled method."),
        "a disabled plugin never instructs a Planner, even when referenced:\n{prompt}"
    );
}

#[tokio::test]
async fn plugin_instructions_read_from_row_before_host_boot() {
    let boot = boot().await;
    let host = host_before_boot(&boot);
    assert!(host.registry().get("k2-row").is_none());
    assert!(host.running_plugin_ids().await.is_empty());
    install_row(&boot, "k2-row", "Stored row method.", true).await;
    reference_by_series(&boot, "k2-row").await;

    let prompt = prompt(&boot, &host).await;
    assert!(
        prompt.ends_with(&format!("\n\n{}", block("k2-row", "Stored row method."))),
        "the text comes from the stored row, not the plugin host:\n{prompt}"
    );
}

#[tokio::test]
async fn plugin_instructions_aggregate_never_exceeds_cap() {
    let boot = boot().await;
    let host = host_before_boot(&boot);
    let text = "k".repeat(1990);
    let ids: Vec<String> = ["a", "b", "c"]
        .iter()
        .map(|letter| format!("k2-cap-{letter}-{}", "0".repeat(23)))
        .collect();
    for id in &ids {
        assert_eq!(id.len(), 32);
        install_row(&boot, id, &text, true).await;
        reference_by_table(&boot, id).await;
    }
    assert_eq!(block(&ids[0], &text).len(), 2034);
    assert_eq!(NOTICE.len(), 65);

    let prompt = prompt(&boot, &host).await;
    let start = prompt.find("## Plugin ").expect("a plugin section");
    let appended = &prompt[start..];
    assert_eq!(
        appended,
        format!("{}{NOTICE}", block(&ids[0], &text)),
        "the first block fits in 4,031 B, the second does not, and the notice follows once"
    );
    assert_eq!(appended.len(), 2099);
    assert!(appended.len() <= 4096);
}

#[test]
fn manifest_v4_refuses_planner_instructions() {
    let v4 = manifest(4, "k2-version", "Method.");
    match Manifest::parse(&v4.to_string()) {
        Err(ManifestError::Invalid { field, .. }) => assert_eq!(field, "manifest_version"),
        other => panic!("v4 must refuse `planner_instructions`: {other:?}"),
    }
    let parsed = Manifest::parse(&manifest(5, "k2-version", "Method.").to_string())
        .expect("v5 accepts `planner_instructions`");
    assert_eq!(parsed.to_json()["planner_instructions"], "Method.");
    let long = manifest(5, "k2-version", &"x".repeat(2049));
    match Manifest::parse(&long.to_string()) {
        Err(ManifestError::Invalid { field, .. }) => assert_eq!(field, "planner_instructions"),
        other => panic!("more than 2,048 B must be refused: {other:?}"),
    }
    Manifest::parse(&manifest(5, "k2-version", &"x".repeat(2048)).to_string())
        .expect("exactly 2,048 B is accepted");
}
