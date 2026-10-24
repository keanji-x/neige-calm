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
use calm_truth::db::RepoOutOfDomain as _;
struct Fixture {
    repo: Arc<SqlxRepo>,
    host: Arc<PluginHost>,
    ctx: Arc<AppContext>,
    _tmp: tempfile::TempDir,
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
            _tmp: tmp,
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
