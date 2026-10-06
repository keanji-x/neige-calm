//! #2209 — `neige_user_ask` and `POST /api/tracks/{id}/asks/{ask_id}/answer` through their
//! production entry points: the tool writes one `ask.requested`, the route writes the user's
//! `ask.answered`, and the activity projector shows and closes the ask.
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
use calm_server::event::{AskQuestion, Event, EventBus};
use calm_server::ids::{AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::user_ask::TOOL_USER_ASK;
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

const PLANNER_SESSION_ID: &str = "user-ask-planner-session";

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
            name: "user-ask".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "user ask".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: Some("gitforge".into()),
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
        std::env::temp_dir().join("calm-plugins-data-user-ask"),
        Vec::new(),
        EventBus::new(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
    ));
    host.reconcile_builtins().await.unwrap();
    host.enable("gitforge").await.unwrap();
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

async fn ask(
    boot: &Boot,
    questions: Value,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    call_tool(boot, TOOL_USER_ASK, json!({ "questions": questions })).await
}

/// One question and the `ask_id` the tool returned.
async fn ask_one(boot: &Boot, title: &str) -> i64 {
    let result = ask(boot, json!([{ "title": title }])).await.expect("ask");
    result["ask_id"].as_i64().expect("ask_id")
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

/// `(actor kind, event)` of every row of `kind` on the boot track.
async fn rows(boot: &Boot, kind: &str) -> Vec<(String, Event)> {
    boot.repo
        .events_for_track(boot.track_id.as_str(), &[kind], None)
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            let actor = serde_json::to_value(&row.actor).unwrap()["kind"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            (actor, row.event)
        })
        .collect()
}

async fn post_answer(
    boot: &Boot,
    track: &str,
    ask_id: i64,
    body: Value,
    actor: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/api/tracks/{track}/asks/{ask_id}/answer"))
        .header("content-type", "application/json");
    if let Some(actor) = actor {
        request = request.header(calm_server::actor::Actor::HEADER, actor);
    }
    let resp = boot
        .app
        .clone()
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn answer(boot: &Boot, ask_id: i64, answers: Value) -> (StatusCode, Value) {
    let track = boot.track_id.as_str().to_string();
    post_answer(boot, &track, ask_id, json!({ "answers": answers }), None).await
}

#[tokio::test]
async fn user_ask_writes_one_planner_ask_and_lights_one_item() {
    let boot = boot().await;
    let result = ask(
        &boot,
        json!([
            { "title": "  Merge PR #7 (head abc)?  ", "options": [" Merge ", "Hold"] },
            { "title": "Which region?" },
        ]),
    )
    .await
    .expect("ask");
    let ask_id = result["ask_id"].as_i64().expect("ask_id");
    assert_eq!(result, json!({ "ask_id": ask_id }));

    let questions = vec![
        AskQuestion {
            title: "Merge PR #7 (head abc)?".into(),
            options: vec!["Merge".into(), "Hold".into()],
        },
        AskQuestion {
            title: "Which region?".into(),
            options: Vec::new(),
        },
    ];
    let asked = rows(&boot, "ask.requested").await;
    assert!(
        matches!(asked.as_slice(), [(actor, Event::AskRequested { track_id, questions: q, source_item_id: None })]
            if actor == "AiPlannerSession" && track_id == &boot.track_id && q == &questions),
        "{asked:?}"
    );
    let items = activity_items(&boot).await;
    assert_eq!(
        items,
        vec![calm_server::track_activity::ActivityItem::Ask {
            key: format!("ask:{ask_id}"),
            text: "Merge PR #7 (head abc)? / Which region?".into(),
            at_ms: items[0].at_ms(),
            ask_id,
            questions,
        }]
    );
}

/// Several asks are open at once: there is no one-pending limit.
#[tokio::test]
async fn asks_stay_open_side_by_side() {
    let boot = boot().await;
    let first = ask_one(&boot, "Which region?").await;
    let second = ask_one(&boot, "Which tier?").await;
    assert_ne!(first, second);
    assert_eq!(activity_items(&boot).await.len(), 2);
}

/// A closed track is not refused (the native path could not be refused either); the ask is kept.
#[tokio::test]
async fn user_ask_on_a_closed_track_is_recorded() {
    let boot = boot().await;
    set_closed(&boot, true).await;
    ask_one(&boot, "Reopen to fix the regression?").await;
    assert!(!track_is_open(&boot).await, "an ask changes nothing else");
    assert_eq!(rows(&boot, "ask.requested").await.len(), 1);
    assert_eq!(activity_items(&boot).await.len(), 1);
}

#[tokio::test]
async fn user_ask_refuses_malformed_questions_without_an_event() {
    let boot = boot().await;
    for (args, needle) in [
        (json!({}), "missing `questions` (array)"),
        (json!({ "questions": [] }), "ask 1 to 8 questions"),
        (
            json!({ "questions": [{ "title": "   " }] }),
            "questions[0].title must not be empty",
        ),
        (
            json!({ "questions": [{ "title": "ok", "options": ["a", " "] }] }),
            "questions[0].options[1] must not be empty",
        ),
        (
            json!({ "questions": [{ "title": "ok", "choices": ["a"] }] }),
            "neige_user_ask: questions[0]: unknown argument `choices`; valid: options, title",
        ),
        (
            json!({ "text": "Merge?" }), // the retired ratify argument
            "unknown argument `text`; valid: questions",
        ),
    ] {
        let err = call_tool(&boot, TOOL_USER_ASK, args.clone())
            .await
            .expect_err("must refuse");
        assert_eq!(err.code, -32602, "{args}");
        assert!(
            err.message.starts_with("neige_user_ask: ") && err.message.contains(needle),
            "{args}: {}",
            err.message
        );
    }
    assert!(rows(&boot, "ask.requested").await.is_empty());
}

#[tokio::test]
async fn the_answer_route_records_the_users_answer_and_closes_the_ask() {
    let boot = boot().await;
    let result = ask(
        &boot,
        json!([{ "title": "Merge?", "options": ["Merge", "Hold"] }, { "title": "Why?" }]),
    )
    .await
    .expect("ask");
    let ask_id = result["ask_id"].as_i64().unwrap();

    let (status, body) = answer(&boot, ask_id, json!(["  Merge ", "CI is green"])).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let answered = rows(&boot, "ask.answered").await;
    assert!(
        matches!(answered.as_slice(), [(actor, Event::AskAnswered { ask_id: id, track_id, answers })]
            if actor == "User" && *id == ask_id && track_id == &boot.track_id
                && answers == &vec!["Merge".to_string(), "CI is green".to_string()]),
        "{answered:?}"
    );
    assert!(
        activity_items(&boot).await.is_empty(),
        "the answer closes the ask"
    );
    assert!(track_is_open(&boot).await, "an answer changes nothing else");
}

/// Every refusal is decided in the write transaction and appends nothing.
#[tokio::test]
async fn the_answer_route_refuses_without_an_event() {
    let boot = boot().await;
    let ask_id = ask_one(&boot, "Which region?").await;
    let track = boot.track_id.as_str().to_string();

    for (answers, status, needle) in [
        (
            json!([]),
            StatusCode::BAD_REQUEST,
            "has 1 questions, got 0 answers",
        ),
        (
            json!(["eu", "us"]),
            StatusCode::BAD_REQUEST,
            "has 1 questions, got 2 answers",
        ),
        (
            json!(["  "]),
            StatusCode::BAD_REQUEST,
            "answers[0] must not be empty",
        ),
    ] {
        let (got, body) = answer(&boot, ask_id, answers.clone()).await;
        assert_eq!(got, status, "{answers}: {body}");
        assert!(
            body["error"].as_str().is_some_and(|m| m.contains(needle)),
            "{answers}: {body}"
        );
    }
    let (got, body) = answer(&boot, ask_id + 1000, json!(["eu"])).await;
    assert_eq!(got, StatusCode::NOT_FOUND, "an unknown ask: {body}");
    let (got, body) = post_answer(
        &boot,
        "no-such-track",
        ask_id,
        json!({ "answers": ["eu"] }),
        None,
    )
    .await;
    assert_eq!(got, StatusCode::NOT_FOUND, "an unknown track: {body}");
    for actor in ["ai:codex", "ai:planner-1"] {
        let (got, body) = post_answer(
            &boot,
            &track,
            ask_id,
            json!({ "answers": ["eu"] }),
            Some(actor),
        )
        .await;
        assert_eq!(got, StatusCode::FORBIDDEN, "{actor}: {body}");
    }
    assert!(rows(&boot, "ask.answered").await.is_empty());

    let (got, body) = answer(&boot, ask_id, json!(["eu"])).await;
    assert_eq!(got, StatusCode::NO_CONTENT, "{body}");
    let (got, body) = answer(&boot, ask_id, json!(["us"])).await;
    assert_eq!(got, StatusCode::CONFLICT, "a second answer: {body}");
    assert_eq!(rows(&boot, "ask.answered").await.len(), 1);
}

/// The ask must belong to the track in the path.
#[tokio::test]
async fn an_ask_is_answered_only_on_its_own_track() {
    let boot = boot().await;
    let ask_id = ask_one(&boot, "Which region?").await;
    let other = boot
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: boot.area_id.clone(),
            title: "other".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let (got, body) = post_answer(
        &boot,
        other.id.as_str(),
        ask_id,
        json!({ "answers": ["eu"] }),
        None,
    )
    .await;
    assert_eq!(got, StatusCode::NOT_FOUND, "{body}");
    assert!(rows(&boot, "ask.answered").await.is_empty());
}

/// #2209: the ratify route is gone (the retired tools are in `mcp_tools_list_role_filter`).
#[tokio::test]
async fn the_ratify_route_is_gone() {
    let boot = boot().await;
    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/ratify", boot.planner_card_id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"decision":"grant"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            resp.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ),
        "{}",
        resp.status()
    );
}
