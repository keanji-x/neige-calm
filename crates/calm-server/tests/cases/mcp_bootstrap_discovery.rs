//! Exercise the real registry and wire entry point before a daemon has thread attribution.
#![cfg(unix)]

use crate::support::mcp::{
    CardBoot, boot_shared_daemon_with_planner_thread, connect, handshake, recv_frame, send_frame,
    tools_call_frame, tools_list_frame,
};
use calm_server::builtin_plugins;
use calm_server::mcp_server::build_default_registry;
use calm_server::model::CardRole;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::state::WriteContext;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

const DEV: &str = "gitforge";
const PUBLISH: &str = "plugin_gitforge_publish";

async fn fixture() -> (CardBoot, Arc<PluginHost>) {
    let boot = boot_shared_daemon_with_planner_thread().await;
    let host = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty().with_builtins()),
        boot.repo.clone(),
        boot._tmp.path().join("plugins"),
        boot._tmp.path().join("plugin-data"),
        Vec::new(),
        boot.events.clone(),
        WriteContext::new(boot.card_role_cache.clone(), boot.track_area_cache.clone()),
    ));
    host.reconcile_builtins().await.unwrap();
    for component in builtin_plugins::catalog() {
        host.enable(&component.manifest().id).await.unwrap();
    }
    assert!(boot.plugin_host.set(host.clone()).is_ok());
    (boot, host)
}

async fn bind(boot: &CardBoot, owner: &str) {
    sqlx::query("UPDATE tracks SET plugin_scope = ?1 WHERE id = ?2")
        .bind(owner)
        .bind(boot.track_id.as_str())
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
}

async fn rpc(boot: &CardBoot, token: &str, frame: Value) -> Value {
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    handshake(&mut rd, &mut wr, token).await;
    send_frame(&mut wr, frame).await;
    recv_frame(&mut rd).await
}

async fn list(boot: &CardBoot, token: &str, thread: Option<&str>) -> BTreeSet<String> {
    let frame = match thread {
        Some(thread) => tools_list_frame(2, thread),
        None => json!({"jsonrpc":"2.0", "id":2, "method":"tools/list", "params":{}}),
    };
    let response = rpc(boot, token, frame).await;
    assert!(response.get("error").is_none(), "{response:#}");
    let tools = response["result"]["tools"].as_array().unwrap();
    let names: BTreeSet<_> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), tools.len(), "duplicate descriptors");
    names
}

#[tokio::test]
async fn bootstrap_catalog_covers_every_builtin_bound_role_catalog() {
    let (boot, _host) = fixture().await;
    let daemon = boot.daemon_token.as_deref().unwrap();
    let initial = list(&boot, daemon, None).await;
    let unresolved = list(&boot, daemon, Some("not-yet-attributed")).await;
    assert_eq!(initial, unresolved);
    let registry = build_default_registry();
    for component in builtin_plugins::catalog() {
        let owner = &component.manifest().id;
        bind(&boot, owner).await;
        for role in [CardRole::Planner, CardRole::Worker] {
            crate::support::mcp::set_persisted_card_role(boot.repo.as_ref(), &boot.card_id, role)
                .await;
            let bound = list(&boot, daemon, Some(&boot.thread_id)).await;
            let missing: Vec<_> = bound.difference(&initial).collect();
            assert!(
                missing.is_empty(),
                "bootstrap omitted bound tools: {missing:?}"
            );
            for descriptor in registry.descriptors_listed_for(role) {
                if builtin_plugins::owner(&descriptor.name)
                    .is_some_and(|p| p.manifest().id == *owner)
                {
                    assert!(
                        bound.contains(&descriptor.name),
                        "missing native {}",
                        descriptor.name
                    );
                }
            }
            for tool in &component.manifest().exposes_tools {
                assert!(bound.contains(&format!("plugin_{owner}_{}", tool.name)));
            }
        }
    }
}

#[tokio::test]
async fn bootstrap_discovery_does_not_grant_native_call_authority() {
    let (boot, host) = fixture().await;
    let daemon = boot.daemon_token.as_deref().unwrap();
    let initial = list(&boot, daemon, None).await;
    assert!(initial.contains(PUBLISH));
    // A known unbound Track remains restricted even though bootstrap discovery is wide.
    let unbound = list(&boot, daemon, Some(&boot.thread_id)).await;
    assert!(!unbound.contains(PUBLISH));
    let denied = rpc(
        &boot,
        daemon,
        tools_call_frame(3, PUBLISH, &boot.thread_id, json!({})),
    )
    .await;
    assert_eq!(denied["error"]["code"], -32601, "{denied:#}");
    bind(&boot, DEV).await;
    // Publication reaches its owning handler but keeps argument validation.
    let response = rpc(
        &boot,
        daemon,
        tools_call_frame(3, PUBLISH, &boot.thread_id, json!({})),
    )
    .await;
    assert_eq!(response["error"]["code"], -32602, "{response:#}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("idempotency_key")
    );
    let no_identity = rpc(
        &boot,
        daemon,
        json!({"jsonrpc":"2.0", "id":3, "method":"tools/call",
        "params":{"name":PUBLISH, "arguments":{}}}),
    )
    .await;
    assert_eq!(no_identity["error"]["code"], -32602, "{no_identity:#}");
    let unknown = rpc(
        &boot,
        daemon,
        tools_call_frame(3, PUBLISH, "not-yet-attributed", json!({})),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32601, "{unknown:#}");
    crate::support::mcp::set_persisted_card_role(
        boot.repo.as_ref(),
        &boot.card_id,
        CardRole::Worker,
    )
    .await;
    let worker = list(&boot, daemon, Some(&boot.thread_id)).await;
    assert!(!worker.contains(PUBLISH));
    let response = rpc(
        &boot,
        daemon,
        tools_call_frame(3, PUBLISH, &boot.thread_id, json!({})),
    )
    .await;
    assert_eq!(response["error"]["code"], -32403, "{response:#}");
    crate::support::mcp::set_persisted_card_role(
        boot.repo.as_ref(),
        &boot.card_id,
        CardRole::Planner,
    )
    .await;
    host.disable(DEV).await.unwrap();
    let stopped = list(&boot, daemon, None).await;
    assert!(!stopped.contains(PUBLISH));
    let response = rpc(
        &boot,
        daemon,
        tools_call_frame(3, PUBLISH, &boot.thread_id, json!({})),
    )
    .await;
    assert_eq!(response["error"]["code"], -32503, "{response:#}");
    assert!(
        !stopped
            .iter()
            .any(|name| name.starts_with("plugin_gitforge_"))
    );
}

#[tokio::test]
async fn builtin_calls_reject_cross_session_and_expired_identity_after_discovery() {
    use calm_server::db::sqlite::{
        session_bind_attribution_tx, session_mark_superseded_runtime_tx,
        session_projection_active_for_card_tx,
    };
    use calm_server::session_projection_repo::{AgentProvider, ThreadAttribution};

    let (boot, _host) = fixture().await;
    bind(&boot, DEV).await;
    let (mut rd, mut wr) = connect(&boot.socket_path).await;
    handshake(&mut rd, &mut wr, &boot.raw_token).await;
    send_frame(
        &mut wr,
        json!({"jsonrpc":"2.0", "id":2, "method":"tools/list", "params":{}}),
    )
    .await;
    let listed = recv_frame(&mut rd).await;
    assert!(
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == PUBLISH)
    );

    let mut tx = boot.sqlx.pool().begin().await.unwrap();
    let foreign = session_projection_active_for_card_tx(&mut tx, &boot.other_card_id)
        .await
        .unwrap()
        .unwrap();
    session_bind_attribution_tx(
        &mut tx,
        &foreign.id,
        ThreadAttribution {
            worker_session_id: foreign.id.clone(),
            provider: AgentProvider::Codex,
            thread_id: Some("foreign-thread".into()),
            session_id: None,
            active_turn_id: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    send_frame(
        &mut wr,
        tools_call_frame(3, PUBLISH, "foreign-thread", json!({})),
    )
    .await;
    let rejected = recv_frame(&mut rd).await;
    assert_eq!(rejected["error"]["code"], -32602, "{rejected:#}");
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap()
            .contains("other than this connection")
    );
    let mut tx = boot.sqlx.pool().begin().await.unwrap();
    session_mark_superseded_runtime_tx(&mut tx, &boot.session_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    send_frame(
        &mut wr,
        json!({"jsonrpc":"2.0", "id":4, "method":"tools/call",
        "params":{"name":PUBLISH, "arguments":{}}}),
    )
    .await;
    let rejected = recv_frame(&mut rd).await;
    assert_eq!(rejected["error"]["code"], -32401, "{rejected:#}");
    let expired = rpc(
        &boot,
        boot.daemon_token.as_deref().unwrap(),
        tools_call_frame(5, PUBLISH, &boot.thread_id, json!({})),
    )
    .await;
    assert_eq!(expired["error"]["code"], -32601, "{expired:#}");
}
