//! #2087 B4 (§4 closed input, §5 codes): every kernel tool, called through the kernel socket by an
//! authenticated Planner session, refuses an unknown top-level key with `-32602`, led by its own
//! name and naming every key its declared schema accepts.

#![cfg(unix)]

use std::sync::Arc;

use crate::support;

use calm_server::mcp_server::build_default_registry;
use calm_server::model::CardRole;
use serde_json::{Value, json};
use support::mcp::{boot_with_role, call_tool_via_socket};

/// The kernel registry's tools, each with the keys its declared schema accepts, by name.
fn kernel_tools() -> Vec<(String, Vec<String>, Value)> {
    let mut tools: Vec<_> = build_default_registry()
        .descriptors()
        .into_iter()
        .map(|descriptor| {
            let mut keys: Vec<String> = descriptor.input_schema["properties"]
                .as_object()
                .map(|properties| properties.keys().cloned().collect())
                .unwrap_or_default();
            keys.sort();
            (descriptor.name, keys, descriptor.input_schema)
        })
        .collect();
    tools.sort_by(|a, b| a.0.cmp(&b.0));
    tools
}

#[tokio::test]
async fn every_kernel_tool_refuses_unknown_arguments() {
    let boot = boot_with_role(CardRole::Planner).await;
    // The built-in plugins' native tools sit behind their plugin's fence; bind the host that runs
    // them so the closed input is reached.
    let host = Arc::new(calm_server::plugin_host::PluginHost::new_full(
        Arc::new(calm_server::plugin_host::PluginRegistry::empty().with_builtins()),
        boot.repo.clone(),
        boot._tmp.path().join("plugins"),
        boot._tmp.path().join("plugin-data"),
        Vec::new(),
        boot.events.clone(),
        calm_server::state::WriteContext::new(
            boot.card_role_cache.clone(),
            boot.track_area_cache.clone(),
        ),
    ));
    host.reconcile_builtins().await.unwrap();
    for plugin in calm_server::builtin_plugins::catalog() {
        host.enable(&plugin.manifest().id).await.unwrap();
    }
    assert!(boot.plugin_host.set(host).is_ok());
    let pool = boot.repo.sqlite_pool().expect("sqlite-backed boot");

    let tools = kernel_tools();
    assert!(
        tools.len() >= 40,
        "anti-vacuity: {} kernel tools",
        tools.len()
    );
    for (name, keys, schema) in &tools {
        assert_eq!(
            schema["additionalProperties"],
            json!(false),
            "{name}: the declared schema is closed like its input"
        );
        // A native tool is reachable on a Track its plugin owns.
        let scope = calm_server::builtin_plugins::owner(name).map(|p| p.manifest().id.clone());
        sqlx::query("UPDATE tracks SET plugin_scope = ?1 WHERE id = ?2")
            .bind(scope)
            .bind(boot.track_id.as_str())
            .execute(&pool)
            .await
            .unwrap();

        let resp = call_tool_via_socket(
            &boot.socket_path,
            &boot.raw_token,
            &boot.thread_id,
            7,
            name,
            json!({ "zz": 1 }),
        )
        .await;
        let error = &resp["error"];
        let valid = if keys.is_empty() {
            "none".to_string()
        } else {
            keys.join(", ")
        };
        assert_eq!(
            (&error["code"], error["message"].as_str()),
            (
                &json!(-32602),
                Some(format!("{name}: unknown argument `zz`; valid: {valid}").as_str())
            ),
            "{name}: {resp}"
        );
    }
}
