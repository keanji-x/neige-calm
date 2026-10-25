//! Production boot follows daemon readiness before reloading the changed kernel catalog.
use super::*;
use crate::mcp_server::wiring::{MCP_SERVER_KEY, MCP_TOOLSET_ENV};

#[tokio::test]
async fn boot_catalog_follower_updates_the_entry_before_reloading() {
    let fixture = Fixture::new().await;
    let (state, _app, _cookie) = fixture.application().await;
    let config_path = state.codex.shared_codex_home.path().join("config.toml");
    let generation = || {
        let config: toml::Value = std::fs::read_to_string(&config_path)
            .unwrap()
            .parse()
            .unwrap();
        config["mcp_servers"][MCP_SERVER_KEY]["env"]
            .get(MCP_TOOLSET_ENV)
            .cloned()
    };
    let before = generation();
    let (_client, mut peer) = connect(&state.shared_codex_appserver).await;
    let reload = native_request(&mut peer).await;
    assert_eq!(reload["method"], "config/mcpServer/reload");
    assert!(reload["params"].is_null());
    let at_reload = generation();
    assert!(at_reload.is_some(), "reload requires a catalog generation");
    assert_ne!(at_reload, before, "the entry must change before reload");
    send(&mut peer, json!({"id":reload["id"],"result":{}})).await;
    fixture.shutdown_process(&state).await;
}
