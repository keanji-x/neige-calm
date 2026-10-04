use super::*;
use crate::card_role_cache::CardRoleCache;
use crate::db::prelude::*;
use crate::db::sqlite::SqlxRepo;
use crate::event::EventBus;
use crate::model::{CardRole, NewArea, NewCard, NewTrack, Track};
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::session_projection_repo::AgentProvider;
use crate::state::WriteContext;
use crate::track_area_cache::TrackAreaCache;

const ID: &str = "dev.neige.git-forge";
const NATIVE: [&str; 1] = ["calm.track.publish"];
struct Fixture {
    repo: Arc<SqlxRepo>,
    host: Arc<PluginHost>,
    ctx: Arc<AppContext>,
    _tmp: tempfile::TempDir,
}
impl Fixture {
    async fn new() -> Self {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let events = EventBus::new();
        let write = WriteContext::new(CardRoleCache::new(), TrackAreaCache::new());
        let host = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty().with_builtins()),
            repo.clone(),
            tmp.path().join("plugins"),
            tmp.path().join("data"),
            Vec::new(),
            events.clone(),
            write.clone(),
        ));
        host.reconcile_builtins().await.unwrap();
        host.enable(ID).await.unwrap();
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
    async fn track(&self, scope: Option<&str>) -> Track {
        let area = self
            .repo
            .area_create(NewArea {
                name: "builtin acceptance".into(),
                color: "#112233".into(),
                sort: None,
            })
            .await
            .unwrap();
        self.repo.track_create(NewTrack {
            area_id: area.id, title: "builtin acceptance".into(), sort: None, cwd: String::new(),
            template_id: scope.map(|id| if id == ID { "issue-development".into() } else { "investigation".into() }),
            template_input: scope.map(|_| json!({"issue_url":"https://github.com/example/repo/issues/1","repo":"example/repo","issue_number":1})),
            plugin_scope: scope.map(str::to_owned), attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        }).await.unwrap()
    }
    fn identity(&self, track: &Track) -> ToolCallIdentity {
        ToolCallIdentity {
            card_id: "test-card".into(),
            role: CardRole::Planner,
            provider: AgentProvider::Codex,
            session_id: "test-session".into(),
            track_id: Some(track.id.to_string()),
            area_id: track.area_id.to_string(),
            thread_id: "test-thread".into(),
        }
    }
    async fn tool_names(&self, identity: &ToolCallIdentity) -> Vec<String> {
        let registry = crate::mcp_server::build_default_registry();
        let mut descriptors = registry.descriptors_for_role(identity.role);
        let scope = plugin_scope_for_track(&self.ctx, identity.track_id.as_deref()).await;
        crate::mcp_server::transport::extend_plugin_tool_descriptors_for_role(
            &self.ctx,
            &mut descriptors,
            identity.role,
            &scope,
        )
        .await;
        descriptors.into_iter().map(|d| d.name).collect()
    }
}

#[tokio::test]
async fn builtin_lifecycle_has_no_process_token_or_supervisor() {
    let fx = Fixture::new().await;
    let state = fx.host.status(ID).await.unwrap();
    assert_eq!(
        state.status,
        crate::plugin_host::PluginRuntimeStatus::Running
    );
    assert_eq!(state.pid, None);
    assert!(fx.host.mcp_client(ID).await.is_none());
    assert!(matches!(
        fx.host.connector_client(ID).await,
        Some(crate::plugin_host::ConnectorClient::Builtin(_))
    ));
    let tokens: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plugin_tokens")
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(tokens, 0);
    fx.host.disable(ID).await.unwrap();
    fx.host.reconcile_builtins().await.unwrap();
    assert!(!fx.repo.plugin_get_by_id(ID).await.unwrap().unwrap().enabled);
    assert!(fx.host.running_plugin_ids().await.is_empty());
    fx.host.enable(ID).await.unwrap();
    assert_eq!(fx.host.status(ID).await.unwrap().pid, None);
    fx.host.reload(ID).await.unwrap();
    assert_eq!(fx.host.status(ID).await.unwrap().pid, None);
    assert!(fx.host.uninstall(ID).await.is_err());
}

#[tokio::test]
async fn builtin_native_calls_require_bound_dev_and_live_component() {
    let fx = Fixture::new().await;
    let registry = crate::mcp_server::build_default_registry();
    for scope in [None, Some("foreign.plugin"), Some(ID)] {
        let track = fx.track(scope).await;
        let identity = fx.identity(&track);
        for name in NATIVE {
            let error = registry.lookup(name).unwrap()(fx.ctx.clone(), identity.clone(), json!({}))
                .await
                .err()
                .unwrap();
            assert_eq!(
                error.code,
                if scope == Some(ID) { -32602 } else { -32601 },
                "{name} scope={scope:?}: {error:?}"
            );
        }
    }
    let track = fx.track(Some(ID)).await;
    let identity = fx.identity(&track);
    for role in [CardRole::Assistant, CardRole::Worker] {
        let mut caller = identity.clone();
        caller.role = role;
        for name in NATIVE {
            let error = registry.lookup(name).unwrap()(fx.ctx.clone(), caller.clone(), json!({}))
                .await
                .err()
                .unwrap();
            assert_eq!(error.code, -32602, "{name} must reject {role:?}");
            assert!(error.message.contains("tool requires role"), "{error:?}");
        }
    }
    fx.host.disable(ID).await.unwrap();
    for name in NATIVE {
        let error = registry.lookup(name).unwrap()(fx.ctx.clone(), identity.clone(), json!({}))
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, -32002, "stale identity called disabled {name}");
        assert!(error.message.contains("enable it in Settings"));
        let mut assistant = identity.clone();
        assistant.role = CardRole::Assistant;
        let denied = registry.lookup(name).unwrap()(fx.ctx.clone(), assistant, json!({}))
            .await
            .err()
            .unwrap();
        assert_eq!(
            denied.code, -32601,
            "disabled hints must not widen Assistant permissions"
        );
    }
}

#[tokio::test]
async fn builtin_discovery_matches_native_and_git_call_scope() {
    let fx = Fixture::new().await;
    for scope in [None, Some("foreign.plugin"), Some(ID)] {
        let track = fx.track(scope).await;
        let identity = fx.identity(&track);
        let names = fx.tool_names(&identity).await;
        for name in NATIVE {
            assert_eq!(
                names.iter().any(|n| n == name),
                scope == Some(ID),
                "{name} scope={scope:?}"
            );
        }
        assert_eq!(
            names
                .iter()
                .any(|n| n.starts_with("plugin.dev.neige.git-forge_")),
            scope == Some(ID)
        );
    }
    let track = fx.track(Some(ID)).await;
    let identity = fx.identity(&track);
    fx.host.disable(ID).await.unwrap();
    let names = fx.tool_names(&identity).await;
    assert!(names.iter().all(|n| !NATIVE.contains(&n.as_str()) && !n.starts_with("plugin.dev.neige.git-forge_")));
}

#[tokio::test]
async fn builtin_provenance_refuses_disk_replacement_and_install() {
    let fx = Fixture::new().await;
    let dir = fx._tmp.path().join("rogue");
    std::fs::create_dir(&dir).unwrap();
    let mut forged = get(ID).unwrap().manifest().to_json();
    forged["kind"] = json!("app");
    forged["entrypoint"] = json!({"command":"steal"});
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&forged).unwrap(),
    )
    .unwrap();
    assert!(
        fx.host
            .install(Manifest::parse(&forged.to_string()).unwrap(), &dir)
            .await
            .is_err()
    );
    let (loaded, report) = PluginRegistry::load_from_dir(fx._tmp.path()).unwrap();
    assert!(
        loaded.get(ID).is_none(),
        "disk app stole the compiled identity"
    );
    assert!(report.skipped.iter().any(|(p, _)| p == &dir));
    let loaded = loaded.with_builtins();
    assert_eq!(
        loaded.get(ID).unwrap().kind,
        crate::plugin_host::ConnectorKind::Builtin
    );
    assert!(loaded.install_path(ID).is_none());
}

#[tokio::test]
async fn builtin_issue_instructions_remain_documentation_when_dev_is_disabled() {
    let fx = Fixture::new().await;
    let base = crate::planner_card::render_system_prompt(
        crate::planner_card::SeededCardRole::Planner.prompt_template(),
        "test",
    );
    assert!(!base.contains("calm.track.publish"));
    for scope in [None, Some("foreign.plugin"), Some(ID)] {
        let track = fx.track(scope).await;
        let card = fx
            .repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                kind: "codex".into(),
                sort: None,
                payload: json!({}),
                title: None,
            })
            .await
            .unwrap();
        let prompt = crate::operation::planner_harness_start_adapter::planner_instructions(
            fx.repo.as_ref(),
            &fx.host,
            track.id.as_str(),
            card.id.as_str(),
        )
        .await
        .unwrap();
        assert_eq!(
            prompt.contains("## Development capability"),
            scope == Some(ID)
        );
        if scope == Some(ID) {
            fx.host.disable(ID).await.unwrap();
            let prompt = crate::operation::planner_harness_start_adapter::planner_instructions(
                fx.repo.as_ref(),
                &fx.host,
                track.id.as_str(),
                card.id.as_str(),
            )
            .await
            .unwrap();
            assert!(prompt.contains("## Development capability"));
        }
    }
}

#[tokio::test]
async fn builtin_upgrade_preserves_configuration_and_revokes_the_old_token() {
    let fx = Fixture::new().await;
    fx.host.disable(ID).await.unwrap();
    let mut old = get(ID).unwrap().manifest().to_json();
    old["kind"] = json!("app");
    old["entrypoint"] = json!({"command":"bin/git-forge"});
    fx.repo
        .plugin_install(crate::model::NewPlugin {
            id: ID.into(),
            version: "0.1.0".into(),
            install_path: "/previous/install".into(),
            manifest: old,
            enabled: false,
            user_config: json!({}),
        })
        .await
        .unwrap();
    fx.repo
        .plugin_update_user_config(ID, json!({"retained": "operator-value"}))
        .await
        .unwrap();
    fx.repo
        .plugin_token_set(ID, "previous-hash", i64::MAX)
        .await
        .unwrap();
    fx.host.reconcile_builtins().await.unwrap();
    let row = fx.repo.plugin_get_by_id(ID).await.unwrap().unwrap();
    assert!(!row.enabled);
    assert_eq!(row.user_config, json!({"retained":"operator-value"}));
    assert_eq!(row.manifest["kind"], "builtin");
    assert!(fx.repo.plugin_token_get(ID).await.unwrap().is_none());
}

#[tokio::test]
async fn builtin_legacy_issue_documentation_preserves_snapshot_and_authority() {
    let fx = Fixture::new().await;
    let plain = fx.track(None).await;
    sqlx::query("UPDATE tracks SET template_id='issue-development' WHERE id=?")
        .bind(plain.id.as_str())
        .execute(fx.repo.pool())
        .await
        .unwrap();
    let card = fx.repo.card_create(NewCard {
        track_id: plain.id.clone(), kind: "codex".into(), sort: None,
        payload: json!({"planner_harness":true,"template_context":{"version":1,"title":"Original method","body":"Legacy café method\n"}}),
        title: None,
    }).await.unwrap();
    let original: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?")
        .bind(card.id.as_str())
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    fx.host.disable(ID).await.unwrap();
    let prompt = crate::operation::planner_harness_start_adapter::planner_instructions(
        fx.repo.as_ref(),
        &fx.host,
        plain.id.as_str(),
        card.id.as_str(),
    )
    .await
    .unwrap();
    assert!(prompt.contains("## Development capability"));
    assert!(prompt.contains("Legacy café method"));
    let saved: String = sqlx::query_scalar("SELECT payload FROM cards WHERE id=?")
        .bind(card.id.as_str())
        .fetch_one(fx.repo.pool())
        .await
        .unwrap();
    assert_eq!(saved, original);
    let track = fx.repo.track_get(plain.id.as_str()).await.unwrap().unwrap();
    assert!(track.plugin_scope.is_none());
    assert!(track.template_input.is_none());
    let identity = fx.identity(&track);
    assert!(
        !fx.tool_names(&identity)
            .await
            .iter()
            .any(|name| NATIVE.contains(&name.as_str()))
    );
}
