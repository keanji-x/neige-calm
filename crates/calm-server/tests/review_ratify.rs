#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::RepoEventWrite;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx, session_mark_wave_root_tx};
use calm_server::error::CalmError;
use calm_server::event::{Event, EventBus, RatifyDecision};
use calm_server::ids::{CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::review::{TOOL_RATIFY_REQUEST, TOOL_REVIEW_ROUND};
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{CardRole, NewCard, NewCove, NewWave, WaveLifecycle, WavePatch};
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

const SPEC_SESSION_ID: &str = "review-ratify-spec-session";

struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    repo: Arc<dyn Repo>,
    app: axum::Router,
    cove_id: CoveId,
    wave_id: WaveId,
    spec_card_id: CardId,
}

fn planner_session(id: &str, wave_id: WaveId, card_id: CardId) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(id),
        wave_id,
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

async fn seed_wave_root_session(
    repo: &dyn RepoEventWrite,
    wave_id: &WaveId,
    card_id: &CardId,
    session_id: &str,
) {
    let session = planner_session(session_id, wave_id.clone(), card_id.clone());
    let root_session_id = session.id.clone();
    let wave_id = wave_id.clone();
    calm_server::db::write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            session_insert_tx(tx, session)
                .await
                .map_err(CalmError::from)?;
            session_mark_wave_root_tx(tx, &wave_id, &root_session_id)
                .await
                .map_err(CalmError::from)?;
            Ok(())
        })
    })
    .await
    .expect("seed wave root session");
}

async fn boot() -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "review-ratify".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "review ratify".into(),
            sort: None,
            cwd: String::new(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let wave = repo
        .wave_update(
            wave.id.as_str(),
            WavePatch {
                lifecycle: Some(WaveLifecycle::Working),
                ..WavePatch::default()
            },
        )
        .await
        .unwrap();
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .unwrap();
    seed_wave_root_session(repo.as_ref(), &wave.id, &spec_card.id, SPEC_SESSION_ID).await;

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();

    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-review-ratify"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(card_role_cache.clone(), wave_cove_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );
    let app = calm_server::routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        wave_vcs: repo
            .sqlite_pool()
            .map(calm_truth::wave_vcs_repo::SqlxWaveVcsRepo::shared),
        events,
        write: calm_server::state::WriteContext::new(card_role_cache, wave_cove_cache),
        daemon_token_hash: None,
        gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
    });
    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);

    Boot {
        ctx,
        registry: Arc::new(registry),
        repo,
        app,
        cove_id: cove.id,
        wave_id: wave.id,
        spec_card_id: spec_card.id,
    }
}

fn spec_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        provider: AgentProvider::Codex,
        session_id: SPEC_SESSION_ID.to_string(),
        wave_id: Some(boot.wave_id.as_str().to_string()),
        cove_id: boot.cove_id.as_str().to_string(),
        thread_id: "spec-thread".to_string(),
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
    handler(boot.ctx.clone(), spec_identity(boot), args).await
}

fn valid_round_args(n: u32) -> Value {
    json!({
        "subject": { "phase": "impl", "slice_id": "5b", "pr_number": 760 },
        "head_sha": "abc123",
        "n": n,
        "cap": 3,
        "converged": true,
        "channels": [
            { "role": "reviewer-a", "verdict": "approved" },
            { "role": "reviewer-b", "verdict": "approved" }
        ]
    })
}

async fn events_for_wave(boot: &Boot, kinds: &[&str]) -> Vec<Event> {
    boot.repo
        .events_for_wave(boot.wave_id.as_str(), kinds, None)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.event)
        .collect()
}

async fn set_wave_lifecycle(boot: &Boot, lifecycle: WaveLifecycle) {
    boot.repo
        .wave_update(
            boot.wave_id.as_str(),
            WavePatch {
                lifecycle: Some(lifecycle),
                ..WavePatch::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn review_round_rejects_less_than_two_channels() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REVIEW_ROUND,
        json!({
            "subject": { "phase": "impl", "slice_id": "5b", "pr_number": 760 },
            "n": 1,
            "cap": 3,
            "converged": false,
            "channels": [{ "role": "reviewer-a", "verdict": "changes_requested" }]
        }),
    )
    .await
    .expect_err("single channel must be rejected");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(err.message.contains("two channel"), "{err:?}");
}

#[tokio::test]
async fn review_round_rejects_duplicate_channel_roles() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REVIEW_ROUND,
        json!({
            "subject": { "phase": "impl", "slice_id": "5b", "pr_number": 760 },
            "n": 1,
            "cap": 3,
            "converged": true,
            "channels": [
                { "role": " reviewer-a ", "verdict": "approved" },
                { "role": "reviewer-a", "verdict": "approved" }
            ]
        }),
    )
    .await
    .expect_err("duplicate channel roles must be rejected");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(
        err.message.contains("channel roles must be distinct"),
        "{err:?}"
    );
}

#[tokio::test]
async fn review_round_rejects_n_above_cap() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_REVIEW_ROUND,
        json!({
            "subject": { "phase": "impl", "slice_id": "5b", "pr_number": 760 },
            "n": 4,
            "cap": 3,
            "converged": false,
            "channels": [
                { "role": "reviewer-a", "verdict": "changes_requested" },
                { "role": "reviewer-b", "verdict": "approved" }
            ]
        }),
    )
    .await
    .expect_err("n > cap must be rejected");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(err.message.contains("must be <="), "{err:?}");
}

#[tokio::test]
async fn review_round_accepts_valid_round_and_emits_one_event() {
    let boot = boot().await;
    let out = call_tool(&boot, TOOL_REVIEW_ROUND, valid_round_args(1))
        .await
        .expect("valid review round");
    assert_eq!(out["emitted"], json!(true));

    let events = events_for_wave(&boot, &["review.round"]).await;
    assert_eq!(events.len(), 1, "{events:?}");
    match &events[0] {
        Event::ReviewRound {
            n,
            cap,
            converged,
            idempotency_key,
            ..
        } => {
            assert_eq!(*n, 1);
            assert_eq!(*cap, 3);
            assert!(*converged);
            assert_eq!(
                idempotency_key,
                &format!("review.round:{}:impl:5b:760:1", boot.wave_id)
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn review_round_accepts_monotonic_append() {
    let boot = boot().await;
    call_tool(&boot, TOOL_REVIEW_ROUND, valid_round_args(1))
        .await
        .unwrap();
    let out = call_tool(&boot, TOOL_REVIEW_ROUND, valid_round_args(2))
        .await
        .expect("n=2 after n=1 is monotonic");
    assert_eq!(out["emitted"], json!(true));

    let events = events_for_wave(&boot, &["review.round"]).await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(
        matches!(
            events.as_slice(),
            [
                Event::ReviewRound { n: 1, .. },
                Event::ReviewRound { n: 2, .. }
            ]
        ),
        "{events:?}",
    );
}

#[tokio::test]
async fn review_round_idempotent_resubmit_is_noop() {
    let boot = boot().await;
    call_tool(&boot, TOOL_REVIEW_ROUND, valid_round_args(1))
        .await
        .unwrap();
    let out = call_tool(&boot, TOOL_REVIEW_ROUND, valid_round_args(1))
        .await
        .expect("exact duplicate is idempotent");
    assert_eq!(out["emitted"], json!(false));

    let events = events_for_wave(&boot, &["review.round"]).await;
    assert_eq!(events.len(), 1, "duplicate must not append: {events:?}");
}

#[tokio::test]
async fn review_round_stale_same_n_with_different_payload_is_rejected() {
    let boot = boot().await;
    call_tool(&boot, TOOL_REVIEW_ROUND, valid_round_args(1))
        .await
        .unwrap();
    let err = call_tool(
        &boot,
        TOOL_REVIEW_ROUND,
        json!({
            "subject": { "phase": "impl", "slice_id": "5b", "pr_number": 760 },
            "head_sha": "different",
            "n": 1,
            "cap": 3,
            "converged": true,
            "channels": [
                { "role": "reviewer-a", "verdict": "approved" },
                { "role": "reviewer-b", "verdict": "approved" }
            ]
        }),
    )
    .await
    .expect_err("same n with different payload must be rejected");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(err.message.contains("stale/out-of-order"), "{err:?}");

    let events = events_for_wave(&boot, &["review.round"]).await;
    assert_eq!(events.len(), 1, "stale write must not append: {events:?}");
}

#[tokio::test]
async fn review_round_stale_n_after_later_round_with_different_payload_is_rejected() {
    let boot = boot().await;
    call_tool(&boot, TOOL_REVIEW_ROUND, valid_round_args(1))
        .await
        .unwrap();
    call_tool(&boot, TOOL_REVIEW_ROUND, valid_round_args(2))
        .await
        .unwrap();

    let err = call_tool(
        &boot,
        TOOL_REVIEW_ROUND,
        json!({
            "subject": { "phase": "impl", "slice_id": "5b", "pr_number": 760 },
            "head_sha": "stale-different",
            "n": 1,
            "cap": 3,
            "converged": true,
            "channels": [
                { "role": "reviewer-a", "verdict": "approved" },
                { "role": "reviewer-b", "verdict": "approved" }
            ]
        }),
    )
    .await
    .expect_err("stale n=1 after n=2 must be rejected");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(err.message.contains("stale/out-of-order"), "{err:?}");

    let events = events_for_wave(&boot, &["review.round"]).await;
    assert_eq!(events.len(), 2, "stale write must not append: {events:?}");
}

#[tokio::test]
async fn review_round_gap_is_rejected() {
    let boot = boot().await;
    call_tool(&boot, TOOL_REVIEW_ROUND, valid_round_args(1))
        .await
        .unwrap();

    let err = call_tool(&boot, TOOL_REVIEW_ROUND, valid_round_args(3))
        .await
        .expect_err("n=3 after n=1 leaves a gap");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(err.message.contains("stale/out-of-order"), "{err:?}");

    let events = events_for_wave(&boot, &["review.round"]).await;
    assert_eq!(events.len(), 1, "gap write must not append: {events:?}");
}

#[tokio::test]
async fn ratify_request_emits_event_and_flips_working_to_blocked() {
    let boot = boot().await;
    call_tool(
        &boot,
        TOOL_RATIFY_REQUEST,
        json!({ "reason": "cap_exhausted" }),
    )
    .await
    .expect("ratify request");

    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Blocked);
    let events = events_for_wave(&boot, &["ratify.requested"]).await;
    assert!(
        matches!(events.as_slice(), [Event::RatifyRequested { reason, .. }] if reason == "cap_exhausted")
    );
}

async fn post_ratify(boot: &Boot, decision: &str) -> (StatusCode, Value) {
    let body = serde_json::to_vec(&json!({
        "decision": decision,
        "message": format!("human says {decision}")
    }))
    .unwrap();
    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{}/ratify", boot.spec_card_id))
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
async fn ratify_route_rejects_non_blocked_wave_for_all_decisions_without_event() {
    let boot = boot().await;

    for decision in ["grant", "deny"] {
        let (status, body) = post_ratify(&boot, decision).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], json!("conflict"));
        assert!(
            body["error"].as_str().is_some_and(
                |message| message.contains("ratify: wave is not awaiting ratification")
            ),
            "{body}",
        );

        let events = events_for_wave(&boot, &["ratify.resolved"]).await;
        assert!(
            events.is_empty(),
            "rejected {decision} must not append: {events:?}"
        );
    }
}

#[tokio::test]
async fn ratify_route_grant_emits_resolved_and_flips_blocked_to_working() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Blocked).await;

    let (status, body) = post_ratify(&boot, "grant").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["decision"], json!("grant"));

    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Working);
    let events = events_for_wave(&boot, &["ratify.resolved"]).await;
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

#[tokio::test]
async fn ratify_route_deny_emits_resolved_and_stays_blocked() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Blocked).await;

    let (status, body) = post_ratify(&boot, "deny").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["decision"], json!("deny"));

    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Blocked);
    let events = events_for_wave(&boot, &["ratify.resolved"]).await;
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
