//! Complete operator fixtures for catalogs that exercise all running compiled components.
#![cfg(unix)]
use super::opencode_history as history;
use crate::support::mcp::CardBoot;
use calm_server::{builtin_plugins, state::WriteContext};
pub use history::HistoryHost;

pub async fn running(boot: &CardBoot) -> HistoryHost {
    let directory = boot._tmp.path().join("history");
    std::fs::create_dir_all(&directory).unwrap();
    let history = history::boot(
        boot.repo.clone(),
        boot.events.clone(),
        WriteContext::new(boot.card_role_cache.clone(), boot.track_area_cache.clone()),
        &directory,
        true,
    )
    .await;
    history.host.reconcile_builtins().await.unwrap();
    // A required operator configuration is explicit even in all-components fixtures.
    let import = directory.join("inspection.json");
    std::fs::write(&import,serde_json::json!({"schema_version":1,"enabled":true,"identity":"user","profile":"fixture","self_open_id":"ou_fixture","interval_minutes":30,"overlap_minutes":5,"monitor_sources":["group"],"group":{"chat_id":"oc_fixture"},"source_watermarks":{"group":"2026-11-01T00:00:00+08:00"},"tracked_threads":[],"notified_message_ids":[],"handled_message_ids":[],"acknowledged_in_chat_message_ids":[],"pending_items":[]}).to_string()).unwrap();
    boot.repo.plugin_update_user_config("feishuwatch",serde_json::json!({"track_id":boot.track_id,"cli_path":"/usr/bin/false","home":directory,"profile":"fixture","self_open_id":"ou_fixture","import_state_path":import})).await.unwrap();
    for component in builtin_plugins::catalog() {
        history.host.enable(&component.manifest().id).await.unwrap();
    }
    assert!(boot.plugin_host.set(history.host.clone()).is_ok());
    history
}
