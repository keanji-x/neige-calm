#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::RepoEventWrite;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx, session_mark_track_root_tx};
use calm_server::error::CalmError;
use calm_server::event::{Event, EventBus, RatifyDecision};
use calm_server::ids::{AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::review::TOOL_RATIFY_REQUEST;
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, TrackPatch};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::session_projection_repo::AgentProvider;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const PLANNER_SESSION_ID: &str = "review-ratify-planner-session";

struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    repo: Arc<dyn Repo>,
    app: axum::Router,
    area_id: AreaId,
    track_id: TrackId,
    planner_card_id: CardId,
    // Shared with the activity projector in `activity_items`.
    events: EventBus,
    card_role_cache: CardRoleCache,
    track_area_cache: calm_server::track_area_cache::TrackAreaCache,
}

fn planner_session(id: &str, track_id: TrackId, card_id: CardId) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(id),
        track_id,
        provider: WorkerProviderKind::Codex,
        mode: SessionMode::Resumable,
        contract: WorkerContract::Planner,
        parent_session_id: None,
        requester_session_id: None,
        state: WorkerSessionState::Starting,
        mcp_token_hash: None,
        thread_id: None,
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        card_id: Some(card_id),
        handle_state_json: None,
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    }
}

async fn seed_track_root_session(
    repo: &dyn RepoEventWrite,
    track_id: &TrackId,
    card_id: &CardId,
    session_id: &str,
) {
    let session = planner_session(session_id, track_id.clone(), card_id.clone());
    let root_session_id = session.id.clone();
    let track_id = track_id.clone();
    calm_server::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            session_insert_tx(tx, session)
                .await
                .map_err(CalmError::from)?;
            session_mark_track_root_tx(tx, &track_id, &root_session_id)
                .await
                .map_err(CalmError::from)?;
            Ok(())
        })
    })
    .await
    .expect("seed track root session");
}

async fn boot() -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "review-ratify".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "review ratify".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: Some("dev.neige.git-forge".into()),
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
            sort: None,
            payload: serde_json::json!({"planner_provider": "codex"}),
        })
        .await
        .unwrap();
    seed_track_root_session(
        repo.as_ref(),
        &track.id,
        &planner_card.id,
        PLANNER_SESSION_ID,
    )
    .await;

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
    crate::support::mcp::set_persisted_card_role(
        repo.as_ref(),
        planner_card.id.as_str(),
        CardRole::Planner,
    )
    .await;
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();

    let host = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty().with_builtins()),
        repo.clone(),
        PathBuf::new(),
        std::env::temp_dir().join("calm-plugins-data-review-ratify"),
        Vec::new(),
        EventBus::new(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
    ));
    host.reconcile_builtins().await.unwrap();
    host.enable("dev.neige.git-forge").await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        host.clone(),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
    );
    let app = calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let ctx = Arc::new(AppContext {
        terminal_interaction: Arc::new(tokio::sync::OnceCell::new()),
        repo: route_repo,
        track_vcs: repo
            .sqlite_pool()
            .map(calm_truth::track_vcs_repo::SqlxTrackVcsRepo::shared),
        events: events.clone(),
        write: calm_server::state::WriteContext::new(
            card_role_cache.clone(),
            track_area_cache.clone(),
        ),
        daemon_token_hash: None,
        gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
        track_creator: Arc::new(tokio::sync::OnceCell::new()),
        scheduler_poke: Arc::new(tokio::sync::OnceCell::new()),
        series_resolver: Arc::new(calm_server::report_series::SeriesResolver::new_unstarted(
            None,
        )),
        plugin_results: Arc::new(calm_server::plugin_results::PluginResults::new()),
        read_ledger: Arc::new(calm_server::report_read_ledger::ReadLedger::new()),
        preview: Arc::new(calm_server::preview::PreviewRegistry::disabled()),
        sqlite_pool: repo.sqlite_pool(),
    });
    assert!(ctx.plugin_host.set(host).is_ok());
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);

    Boot {
        ctx,
        registry: Arc::new(registry),
        repo,
        app,
        area_id: area.id,
        track_id: track.id,
        planner_card_id: planner_card.id,
        events,
        card_role_cache,
        track_area_cache,
    }
}

fn planner_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.planner_card_id.as_str().to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: PLANNER_SESSION_ID.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "planner-thread".to_string(),
    }
}

async fn call_tool(
    boot: &Boot,
    name: &str,
    args: Value,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    let handler = boot
        .registry
        .lookup(name)
        .unwrap_or_else(|| panic!("tool not registered: {name}"));
    handler(boot.ctx.clone(), planner_identity(boot), args)
        .await
        .map(calm_server::mcp_server::result::ToolResult::into_structured)
}

async fn request_ratification(
    boot: &Boot,
    reason: &str,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    call_tool(boot, TOOL_RATIFY_REQUEST, json!({ "reason": reason })).await
}

async fn events_for_track(boot: &Boot, kinds: &[&str]) -> Vec<Event> {
    boot.repo
        .events_for_track(boot.track_id.as_str(), kinds, None)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.event)
        .collect()
}

async fn set_closed(boot: &Boot, closed: bool) {
    boot.repo
        .track_update(
            boot.track_id.as_str(),
            TrackPatch {
                closed: Some(closed),
                ..TrackPatch::default()
            },
        )
        .await
        .unwrap();
}

async fn track_is_open(boot: &Boot) -> bool {
    boot.repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap()
        .is_open()
}

/// The activity items the production projector computes for the boot track.
async fn activity_items(boot: &Boot) -> Vec<calm_server::track_activity::ActivityItem> {
    let projector = calm_server::track_activity::TrackActivityProjector::new(
        boot.repo.clone(),
        boot.events.clone(),
        calm_server::state::WriteContext::new(
            boot.card_role_cache.clone(),
            boot.track_area_cache.clone(),
        ),
        calm_server::harness::HarnessRegistry::new(),
        calm_server::terminal_renderer::TerminalRendererRegistry::new(),
    )
    .expect("sqlite-backed repo");
    match projector
        .recompute_track(boot.track_id.as_str())
        .await
        .unwrap()
    {
        calm_server::track_activity::Recompute::NoTrack => panic!("the boot track vanished"),
        calm_server::track_activity::Recompute::Unchanged(p)
        | calm_server::track_activity::Recompute::Written(p) => p.items,
    }
}

#[tokio::test]
async fn ratify_request_raises_an_ask_and_resolve_clears_it() {
    let boot = boot().await;
    request_ratification(&boot, "merge_hold: pr #760 at abc123")
        .await
        .expect("ratify request");

    assert!(track_is_open(&boot).await, "a ratify request flips nothing");
    let events = events_for_track(&boot, &["ratify.requested"]).await;
    assert!(
        matches!(events.as_slice(), [Event::RatifyRequested { reason, .. }] if reason == "merge_hold: pr #760 at abc123")
    );
    let items = activity_items(&boot).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].key.starts_with("ask:ratify:"), "{items:?}");
    assert_eq!(items[0].text, "merge_hold: pr #760 at abc123");

    let (status, body) = post_ratify(&boot, "grant").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = activity_items(&boot).await;
    assert!(items.is_empty(), "the resolution clears the ask: {items:?}");
}

#[tokio::test]
async fn ratify_request_refuses_a_closed_track() {
    let boot = boot().await;
    set_closed(&boot, true).await;

    let err = request_ratification(&boot, "ask after close")
        .await
        .expect_err("a closed track must refuse a ratify request");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(err.message.contains("the track is closed"), "{err:?}");

    let events = events_for_track(&boot, &["ratify.requested"]).await;
    assert!(
        events.is_empty(),
        "rejected request must not append: {events:?}"
    );
}

#[tokio::test]
async fn ratify_request_rejects_duplicate_pending_request_without_second_event() {
    let boot = boot().await;
    request_ratification(&boot, "merge_hold: pr #760 at abc123")
        .await
        .expect("first request");

    let err = request_ratification(&boot, "merge_hold retry")
        .await
        .expect_err("pending request must reject duplicate");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(
        err.message.contains("a ratify request is already pending"),
        "{err:?}"
    );

    let events = events_for_track(&boot, &["ratify.requested"]).await;
    assert_eq!(
        events.len(),
        1,
        "duplicate pending request must not append: {events:?}"
    );
}

async fn post_ratify(boot: &Boot, decision: &str) -> (StatusCode, Value) {
    post_ratify_with_message(boot, decision, &format!("human says {decision}")).await
}

async fn post_ratify_with_message(
    boot: &Boot,
    decision: &str,
    message: &str,
) -> (StatusCode, Value) {
    let body = serde_json::to_vec(&json!({
        "decision": decision,
        "message": message
    }))
    .unwrap();
    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/ratify", boot.planner_card_id))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn plain_chat_worker_card_cannot_ratify() {
    let boot = boot().await;
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "plain_chat"}),
        })
        .await
        .unwrap();
    boot.card_role_cache
        .insert(card.id.clone(), CardRole::Worker, boot.track_id.clone());
    let body = serde_json::to_vec(&json!({"decision": "grant"})).unwrap();
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/ratify", card.id))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|message| message.contains("not a planner codex card")),
        "{body}"
    );
}

#[tokio::test]
async fn ratify_route_rejects_non_pending_track_for_all_decisions_without_event() {
    let boot = boot().await;

    for decision in ["grant", "deny"] {
        let (status, body) = post_ratify(&boot, decision).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], json!("conflict"));
        assert!(
            body["error"].as_str().is_some_and(
                |message| message.contains("ratify: track is not awaiting ratification")
            ),
            "{body}",
        );

        let events = events_for_track(&boot, &["ratify.resolved"]).await;
        assert!(
            events.is_empty(),
            "rejected {decision} must not append: {events:?}"
        );
    }
}

#[tokio::test]
async fn ratify_route_rejects_stale_second_verdict_after_grant_without_second_event() {
    let boot = boot().await;
    request_ratification(&boot, "merge_hold: pr #760 at abc123")
        .await
        .expect("ratify request");

    let (status, body) = post_ratify(&boot, "grant").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let events = events_for_track(&boot, &["ratify.resolved"]).await;
    assert!(
        matches!(
            events.as_slice(),
            [Event::RatifyResolved {
                decision: RatifyDecision::Grant,
                ..
            }]
        ),
        "{events:?}",
    );

    for decision in ["grant", "deny"] {
        let (status, body) = post_ratify(&boot, decision).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], json!("conflict"));
        assert!(
            body["error"].as_str().is_some_and(
                |message| message.contains("ratify: track is not awaiting ratification")
            ),
            "{body}",
        );

        let events = events_for_track(&boot, &["ratify.resolved"]).await;
        assert_eq!(
            events.len(),
            1,
            "stale {decision} must not append a second resolution: {events:?}"
        );
    }
}

#[tokio::test]
async fn ratify_route_grant_emits_resolved_and_leaves_the_track_open() {
    let boot = boot().await;
    request_ratification(&boot, "merge_hold: pr #760 at abc123")
        .await
        .expect("ratify request");

    let (status, body) = post_ratify(&boot, "grant").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["decision"], json!("grant"));

    assert!(track_is_open(&boot).await);
    let events = events_for_track(&boot, &["ratify.resolved"]).await;
    assert!(
        matches!(
            events.as_slice(),
            [Event::RatifyResolved {
                decision: RatifyDecision::Grant,
                ..
            }]
        ),
        "{events:?}",
    );
}

/// #1873 item 1: the user's text rides on `ratify.resolved` for either decision, trimmed; a
/// whitespace-only text is no message.
#[tokio::test]
async fn ratify_grant_and_deny_carry_the_message_on_resolved() {
    for (decision, sent, stored) in [
        (
            "grant",
            "  Merge it, then close #1870.\nKeep the PR title.\n",
            Some("Merge it, then close #1870.\nKeep the PR title."),
        ),
        ("deny", "Hold: CI is red.", Some("Hold: CI is red.")),
        ("grant", " \n\t ", None),
    ] {
        let boot = boot().await;
        request_ratification(&boot, "merge_hold: pr #1871")
            .await
            .expect("ratify request");
        let (status, body) = post_ratify_with_message(&boot, decision, sent).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let events = events_for_track(&boot, &["ratify.resolved"]).await;
        let [Event::RatifyResolved { message, .. }] = events.as_slice() else {
            panic!("one ratify.resolved expected: {events:?}");
        };
        assert_eq!(message.as_deref(), stored, "{decision} {sent:?}");
    }
}

#[tokio::test]
async fn ratify_route_deny_emits_resolved_and_leaves_the_track_open() {
    let boot = boot().await;
    request_ratification(&boot, "merge_hold: pr #760 at abc123")
        .await
        .expect("ratify request");

    let (status, body) = post_ratify(&boot, "deny").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["decision"], json!("deny"));

    assert!(track_is_open(&boot).await);
    let events = events_for_track(&boot, &["ratify.resolved"]).await;
    assert!(
        matches!(
            events.as_slice(),
            [Event::RatifyResolved {
                decision: RatifyDecision::Deny,
                ..
            }]
        ),
        "{events:?}",
    );
}
