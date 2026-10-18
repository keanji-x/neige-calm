use super::*;
use crate::{
    card_role_cache::CardRoleCache,
    db::{prelude::*, sqlite::SqlxRepo},
    event::EventBus,
    model::{CardRole, NewArea, NewTrack},
    plugin_host::{PluginHost, PluginRegistry},
    session_projection_repo::AgentProvider,
    state::WriteContext,
    track_area_cache::TrackAreaCache,
};
struct Fixture {
    repo: Arc<SqlxRepo>,
    host: Arc<PluginHost>,
    ctx: Arc<AppContext>,
    tmp: tempfile::TempDir,
    values: Value,
}
impl Fixture {
    async fn new(source: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let events = EventBus::new();
        let write = WriteContext::new(CardRoleCache::new(), TrackAreaCache::new());
        let host = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty().with_builtins()),
            repo.clone(),
            tmp.path().join("plugins"),
            tmp.path().join("data"),
            vec![],
            events.clone(),
            write.clone(),
        ));
        host.reconcile_builtins().await.unwrap();
        let path = tmp.path().join("config.json");
        let password = tmp.path().join("password");
        std::fs::write(&password, "synthetic").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&password, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::write(&path,json!({"sources":[{"source_id":source,"base_url":"http://127.0.0.1:1","credential_ref":"local","directories":["/work/synthetic"]}],"credentials":{"local":{"username":"opencode","password_file":password}},"limits":{"page_messages":50,"max_response_bytes":2097152,"max_result_bytes":131072,"max_parts":128,"max_part_bytes":16384,"request_timeout_ms":1000}}).to_string()).unwrap();
        let values = json!({"history_config_path":path});
        repo.plugin_update_user_config("opencodehistory", values.clone())
            .await
            .unwrap();
        let cell = Arc::new(tokio::sync::OnceCell::new());
        assert!(cell.set(host.clone()).is_ok());
        let ctx = AppContext::new(
            repo.clone(),
            events,
            write,
            None,
            cell,
            Arc::new(tokio::sync::OnceCell::new()),
            tmp.path().join("gates"),
        );
        Self {
            repo,
            host,
            ctx,
            tmp,
            values,
        }
    }
    async fn identity(&self, scope: Option<&str>) -> ToolCallIdentity {
        let area = self
            .repo
            .area_create(
                calm_types::organization::LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "history test".into(),
                    color: "#123456".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = self
            .repo
            .track_create(NewTrack {
                area_id: area.id,
                title: "history test".into(),
                sort: None,
                cwd: String::new(),
                template_id: scope.map(|_| "opencode-history-rebuild".into()),
                template_input: None,
                plugin_scope: scope.map(str::to_owned),
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        ToolCallIdentity {
            card_id: "synthetic-card".into(),
            role: CardRole::Planner,
            provider: AgentProvider::Codex,
            session_id: "synthetic-session".into(),
            track_id: Some(track.id.to_string()),
            area_id: track.area_id.to_string(),
            thread_id: "synthetic-thread".into(),
        }
    }
    async fn call(
        &self,
        identity: ToolCallIdentity,
    ) -> Result<crate::mcp_server::result::ToolResult, RpcError> {
        let registry = crate::mcp_server::build_default_registry();
        let name = registry_name("opencodehistory", "source_ls");
        registry.lookup(&name).unwrap()(self.ctx.clone(), identity, json!({})).await
    }
}
#[test]
fn native_contract_is_optional_and_planner_only() {
    let component = super::super::get("opencodehistory").unwrap();
    assert!(component.can_disable());
    assert_eq!(component.native.descriptors().len(), 4);
    for d in component.native.descriptors() {
        assert_eq!(d.roles, &[CardRole::Planner]);
    }
    assert!(component.manifest().entrypoint.is_none());
}
#[tokio::test]
async fn bound_planner_scope_and_disabled_lifecycle_fence() {
    let fx = Fixture::new("first").await;
    let bound = fx.identity(Some("opencodehistory")).await;
    assert!(
        !fx.repo
            .plugin_get_by_id("opencodehistory")
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
    assert!(fx.call(bound.clone()).await.is_err());
    fx.host.enable("opencodehistory").await.unwrap();
    assert!(fx.call(bound.clone()).await.is_ok());
    assert!(fx.call(fx.identity(None).await).await.is_err());
    fx.host.enable("gitforge").await.unwrap();
    assert!(fx.call(fx.identity(Some("gitforge")).await).await.is_err());
    let registry = crate::mcp_server::build_default_registry();
    for tool in plugin::builtin::opencode_history::descriptors() {
        let name = registry_name(plugin::builtin::opencode_history::PLUGIN_ID, &tool.name);
        for role in [CardRole::Worker, CardRole::Assistant, CardRole::ReportCard] {
            let mut caller = bound.clone();
            caller.role = role;
            let error = registry.lookup(&name).unwrap()(fx.ctx.clone(), caller, json!({}))
                .await
                .unwrap_err();
            assert_eq!(error.code, -32403, "{name}: {error:?}");
            assert!(
                error.message.contains("tool requires role in ["),
                "{name}: {error:?}"
            );
        }
    }
    fx.host.disable("opencodehistory").await.unwrap();
    assert!(fx.call(bound).await.is_err());
    assert!(fx.host.connector_client("opencodehistory").await.is_none());
}
#[tokio::test]
async fn independent_host_runs_snapshot_and_restart_without_process() {
    let a = Fixture::new("first").await;
    let b = Fixture::new("second").await;
    a.host.enable("opencodehistory").await.unwrap();
    b.host.enable("opencodehistory").await.unwrap();
    assert_eq!(a.host.status("opencodehistory").await.unwrap().pid, None);
    assert!(a.host.mcp_client("opencodehistory").await.is_none());
    let first = a.identity(Some("opencodehistory")).await;
    let second = b.identity(Some("opencodehistory")).await;
    let read = serde_json::to_string(&a.call(first.clone()).await.unwrap()).unwrap();
    assert!(read.contains("first"));
    assert!(!read.contains("second"));
    assert!(
        serde_json::to_string(&b.call(second.clone()).await.unwrap())
            .unwrap()
            .contains("second")
    );
    let path = a.values["history_config_path"].as_str().unwrap();
    let mut config: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    config["sources"][0]["source_id"] = json!("restarted");
    std::fs::write(path, config.to_string()).unwrap();
    assert!(
        serde_json::to_string(&a.call(first.clone()).await.unwrap())
            .unwrap()
            .contains("first")
    );
    a.host.reload("opencodehistory").await.unwrap();
    assert!(
        serde_json::to_string(&a.call(first).await.unwrap())
            .unwrap()
            .contains("restarted")
    );
    assert!(
        serde_json::to_string(&b.call(second).await.unwrap())
            .unwrap()
            .contains("second")
    );
    let tokens: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plugin_tokens")
        .fetch_one(a.repo.pool())
        .await
        .unwrap();
    assert_eq!(tokens, 0);
}
#[tokio::test]
async fn invalid_configuration_refuses_running_and_can_recover() {
    let fx = Fixture::new("first").await;
    std::fs::write(fx.tmp.path().join("config.json"), "{}").unwrap();
    assert!(fx.host.enable("opencodehistory").await.is_err());
    assert!(
        !fx.host
            .running_plugin_ids()
            .await
            .contains("opencodehistory")
    );
    assert!(matches!(
        fx.host.status("opencodehistory").await.unwrap().status,
        crate::plugin_host::PluginRuntimeStatus::Unavailable { .. }
    ));
    fx.repo
        .plugin_update_user_config("opencodehistory", json!({}))
        .await
        .unwrap();
    assert!(fx.host.reload("opencodehistory").await.is_err());
}

#[tokio::test]
async fn stateless_builtins_keep_runs_and_native_calendar_entry() {
    let fx = Fixture::new("first").await;
    for id in ["gitforge", "calendar"] {
        fx.host.enable(id).await.unwrap();
        assert!(matches!(
            fx.host.connector_client(id).await,
            Some(crate::plugin_host::ConnectorClient::Builtin(_))
        ));
        assert_eq!(fx.host.status(id).await.unwrap().pid, None);
        fx.host.reload(id).await.unwrap();
    }
    let identity = fx.identity(None).await;
    let registry = crate::mcp_server::build_default_registry();
    let called = registry.lookup("plugin_calendar_ls").unwrap()(
        fx.ctx.clone(),
        identity,
        json!({"from":"2026-10-17","to":"2026-10-18","timezone":"UTC"}),
    )
    .await;
    assert!(called.is_ok(), "{called:?}");
}
#[test]
fn native_text_preserves_exact_serialized_mcp_result() {
    let plugin: CallToolResult = serde_json::from_value(
        json!({"content":[{"type":"text","text":"中文\\\"\n"}],"isError":false}),
    )
    .unwrap();
    let expected = serde_json::to_vec(&plugin).unwrap();
    let native = crate::mcp_server::result::ToolResult::from_plugin(plugin).unwrap();
    assert_eq!(serde_json::to_vec(&native).unwrap(), expected);
}
