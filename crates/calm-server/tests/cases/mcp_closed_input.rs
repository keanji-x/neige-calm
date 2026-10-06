//! #2087 B4 (§4 closed input, §5 codes): every kernel tool and every built-in's compiled plugin
//! tool, called through the kernel socket by an authenticated Planner session, refuses an unknown
//! top-level key with `-32602`, led by its own name and naming every key its declared schema
//! accepts.

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

/// The built-in plugins' native tools sit behind their plugin's fence; bind a host that runs every
/// built-in so a call reaches them.
async fn bind_running_builtins(boot: &support::mcp::CardBoot) {
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
}

async fn scope_track(boot: &support::mcp::CardBoot, scope: Option<&str>) {
    let pool = boot.repo.sqlite_pool().expect("sqlite-backed boot");
    sqlx::query("UPDATE tracks SET plugin_scope = ?1 WHERE id = ?2")
        .bind(scope)
        .bind(boot.track_id.as_str())
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn every_kernel_tool_refuses_unknown_arguments() {
    let boot = boot_with_role(CardRole::Planner).await;
    bind_running_builtins(&boot).await;

    let tools = kernel_tools();
    assert!(
        tools.len() >= 40,
        "anti-vacuity: {} kernel tools",
        tools.len()
    );
    // The built-ins' compiled plugin tools sit in the kernel registry too (#2227): dispatch reaches
    // them before any manifest route, so they refuse under their minted names here.
    for native in [
        "plugin_gitforge_publish",
        "plugin_calendar_add",
        "plugin_calendar_ls",
        "plugin_calendar_set",
        "plugin_calendar_rm",
    ] {
        assert!(tools.iter().any(|(name, ..)| name == native), "{native}");
    }
    for (name, keys, schema) in &tools {
        assert_eq!(
            schema["additionalProperties"],
            json!(false),
            "{name}: the declared schema is closed like its input"
        );
        // A native tool is reachable on a Track its plugin owns.
        let scope = calm_server::builtin_plugins::owner(name).map(|p| p.manifest().id.clone());
        scope_track(&boot, scope.as_deref()).await;

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

/// #2227: dispatch finds a built-in's compiled tool by its minted name in the kernel registry,
/// before any manifest route, and the call reaches the compiled handler: Calendar lists, publish
/// answers its own refusal. The old `neige_` names are unknown; no alias serves them.
#[tokio::test]
async fn compiled_plugin_tools_dispatch_under_their_minted_names() {
    let boot = boot_with_role(CardRole::Planner).await;
    bind_running_builtins(&boot).await;
    let call = |name: &'static str, args: Value| {
        let boot = &boot;
        async move {
            call_tool_via_socket(
                &boot.socket_path,
                &boot.raw_token,
                &boot.thread_id,
                9,
                name,
                args,
            )
            .await
        }
    };

    scope_track(&boot, Some("calendar")).await;
    let listed = call(
        "plugin_calendar_ls",
        json!({"from": "2026-10-01", "to": "2026-10-08", "timezone": "UTC"}),
    )
    .await;
    assert_eq!(
        listed["result"]["structuredContent"],
        json!({"entries": []}),
        "{listed}"
    );

    scope_track(&boot, Some("gitforge")).await;
    let refused = call(
        "plugin_gitforge_publish",
        json!({"idempotency_key": "k", "title": "T", "body": "B"}),
    )
    .await;
    assert_eq!(refused["error"]["code"], json!(-32409), "{refused}");
    assert!(
        refused["error"]["message"].as_str().is_some_and(
            |m| m.starts_with("plugin_gitforge_publish: refused: publish-needs-track-worktree")
        ),
        "{refused}"
    );

    for (old, scope) in [
        ("neige_dev_publish", "gitforge"), // retired-name: rejection input
        ("neige_calendar_ls", "calendar"), // retired-name: rejection input
    ] {
        scope_track(&boot, Some(scope)).await;
        let unknown = call(old, json!({})).await;
        assert_eq!(unknown["error"]["code"], json!(-32601), "{old}: {unknown}");
    }
}
