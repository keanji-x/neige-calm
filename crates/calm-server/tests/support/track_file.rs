use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use calm_server::track_report::TrackReportPayload;
use serde_json::{Value, json};
use tower::ServiceExt;

pub struct Boot {
    pub ctx: Arc<AppContext>,
    pub registry: Arc<ToolRegistry>,
    pub state: AppState,
    pub auth_state: AuthState,
    pub repo: Arc<dyn Repo>,
    pub area_id: AreaId,
    pub track_id: TrackId,
    pub planner_card_id: CardId,
    pub worker_card_id: CardId,
    pub other_track_card_id: CardId,
}

pub async fn boot() -> Boot {
    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let area = repo
        .area_create(NewArea {
            name: "track-file-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "track file test".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let planner_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: Some(0.0),
            payload: json!({ "role": "planner" }),
        })
        .await
        .unwrap();
    let worker_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: Some(1.0),
            payload: json!({ "task": "local" }),
        })
        .await
        .unwrap();
    let report_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();

    let area2 = repo
        .area_create(NewArea {
            name: "track-file-other".into(),
            color: "#0f0".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track2 = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area2.id.clone(),
            title: "other track".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let other_track_card = repo
        .card_create(NewCard {
            track_id: track2.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({ "task": "other-track" }),
        })
        .await
        .unwrap();
    let other_planner_card = repo
        .card_create(NewCard {
            track_id: track2.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: Some(-1.0),
            payload: json!({ "role": "planner" }),
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
    super::mcp::set_persisted_card_role(repo.as_ref(), planner_card.id.as_str(), CardRole::Planner)
        .await;
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, track.id.clone());
    card_role_cache.insert(
        report_card.id.clone(),
        CardRole::ReportCard,
        track.id.clone(),
    );
    card_role_cache.insert(
        other_track_card.id.clone(),
        CardRole::Worker,
        track2.id.clone(),
    );
    super::mcp::set_persisted_card_role(
        repo.as_ref(),
        other_planner_card.id.as_str(),
        CardRole::Planner,
    )
    .await;
    card_role_cache.insert(other_planner_card.id, CardRole::Planner, track2.id.clone());

    let track_area_cache = TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let write =
        calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone());
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        track_vcs: repo
            .sqlite_pool()
            .map(calm_truth::track_vcs_repo::SqlxTrackVcsRepo::shared),
        events: events.clone(),
        write,
        daemon_token_hash: None,
        gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
        task_budget_default: calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
    });

    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    let plugin_data_dir = std::env::temp_dir().join(format!(
        "calm-plugins-data-http-track-file-{}",
        track.id.as_str()
    ));
    // Keep the HTTP router's dispatcher off the fixture bus used by
    // `request_codex()`. These tests materialize worker cards manually;
    // sharing the bus lets the background dispatcher race in and mint a
    // second worker for the same request.
    let app_events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        app_events,
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            plugin_data_dir,
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(CardRoleCache::new(), TrackAreaCache::new()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(track_area_cache),
    );
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });

    Boot {
        ctx,
        registry,
        state,
        auth_state,
        repo,
        area_id: area.id,
        track_id: track.id,
        planner_card_id: planner_card.id,
        worker_card_id: worker_card.id,
        other_track_card_id: other_track_card.id,
    }
}

pub fn app(boot: &Boot) -> axum::Router {
    let protected_rest = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            boot.auth_state.clone(),
            auth::require_session,
        ));
    let public_rest = routes::public_router();
    let auth_router = auth::router().with_state(boot.auth_state.clone());
    axum::Router::new()
        .merge(protected_rest)
        .merge(public_rest)
        .with_state(boot.state.clone())
        .merge(auth_router)
}

pub async fn login(app: &axum::Router) -> String {
    let body = serde_json::to_vec(&json!({
        "username": "alice",
        "password": "hunter2",
    }))
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login must succeed");
    let raw = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie present on login")
        .to_str()
        .unwrap();
    let first = raw.split(';').next().unwrap();
    assert!(first.starts_with(&format!("{SESSION_COOKIE}=")));
    first.to_string()
}

pub async fn call_tool(
    boot: &Boot,
    name: &str,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    let handler = boot
        .registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(boot.ctx.clone(), identity, args).await
}

pub fn planner_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.planner_card_id.as_str().to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: "planner-session".to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "planner-thread".to_string(),
    }
}

#[allow(deprecated)]
pub async fn request_codex(boot: &Boot, key: &str) -> i64 {
    boot.repo
        .log_pure_event(
            ActorId::User,
            EventScope::Track {
                track: boot.track_id.clone(),
                area: boot.area_id.clone(),
            },
            None,
            &boot.ctx.events,
            boot.ctx.write.role_cache(),
            boot.ctx.write.area_cache(),
            Event::CodexWorkerRequested {
                idempotency_key: key.into(),
                goal: format!("goal for {key}"),
                context: json!({ "key": key }),
                acceptance_criteria: Some(format!("accept {key}")),
                agent_message: None,
            },
        )
        .await
        .expect("log codex request")
}

#[allow(deprecated)]
pub async fn materialize_worker(boot: &Boot, key: &str) -> CardId {
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: Some(10.0),
            payload: json!({
                "idempotency_key": key,
                "goal": format!("goal for {key}"),
                "context": { "key": key },
                "acceptance_criteria": format!("accept {key}"),
                "role_request": "codex",
                "prompt": format!("prompt for {key}")
            }),
        })
        .await
        .expect("create worker card");
    boot.ctx
        .write
        .role_cache()
        .insert(card.id.clone(), CardRole::Worker, boot.track_id.clone());
    card.id
}
