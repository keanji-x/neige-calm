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
use calm_server::event::{
    ChannelVerdict, ChannelVerdictKind, Event, EventBus, EventScope, RatifyDecision, ReviewSubject,
};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
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
    // Exposed for tests that seed events via `log_pure_event` (#888 t10/t12/t13).
    events: EventBus,
    card_role_cache: CardRoleCache,
    wave_cove_cache: calm_server::wave_cove_cache::WaveCoveCache,
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
            workflow_input: None,
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
        events: events.clone(),
        write: calm_server::state::WriteContext::new(
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        ),
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
        events,
        card_role_cache,
        wave_cove_cache,
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

async fn request_ratification(
    boot: &Boot,
    reason: &str,
) -> Result<Value, calm_server::plugin_host::mcp::RpcError> {
    call_tool(boot, TOOL_RATIFY_REQUEST, json!({ "reason": reason })).await
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
async fn review_round_rejects_non_token_verdict_at_deserialization() {
    let boot = boot().await;
    for verdict in ["LGTM", "Approved ", "rejected", ""] {
        let err = call_tool(
            &boot,
            TOOL_REVIEW_ROUND,
            json!({
                "subject": { "phase": "impl", "slice_id": "5b", "pr_number": 760 },
                "n": 1,
                "cap": 3,
                "converged": true,
                "channels": [
                    { "role": "reviewer-a", "verdict": verdict },
                    { "role": "reviewer-b", "verdict": "approved" }
                ]
            }),
        )
        .await
        .expect_err("non-token verdict must be rejected");
        assert_eq!(
            err.code,
            calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
        );
        assert!(err.message.contains("invalid args"), "{err:?}");
        assert!(
            err.message.contains("approved") && err.message.contains("changes_requested"),
            "error must name the valid tokens: {err:?}"
        );
    }
    let events = events_for_wave(&boot, &["review.round"]).await;
    assert!(events.is_empty(), "{events:?}");
}

#[tokio::test]
async fn review_round_rejects_converged_with_changes_requested_channel() {
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
                { "role": "reviewer-a", "verdict": "approved" },
                { "role": "reviewer-b", "verdict": "changes_requested" }
            ]
        }),
    )
    .await
    .expect_err("converged=true with a changes_requested channel must be rejected");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(
        err.message
            .contains("requires every channel verdict to be approved"),
        "{err:?}"
    );
    let events = events_for_wave(&boot, &["review.round"]).await;
    assert!(events.is_empty(), "{events:?}");
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
    request_ratification(&boot, "cap_exhausted")
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

#[tokio::test]
async fn ratify_request_rejects_already_blocked_wave_without_requested_event() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Blocked).await;

    let err = request_ratification(&boot, "retry while blocked")
        .await
        .expect_err("blocked wave without pending request must be rejected");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(
        err.message.contains("not in `working`")
            && err.message.contains("ratify request is already pending"),
        "{err:?}"
    );

    let events = events_for_wave(&boot, &["ratify.requested"]).await;
    assert!(
        events.is_empty(),
        "rejected request must not append: {events:?}"
    );
}

#[tokio::test]
async fn ratify_request_rejects_duplicate_pending_request_without_second_event() {
    let boot = boot().await;
    request_ratification(&boot, "cap_exhausted")
        .await
        .expect("first request");
    set_wave_lifecycle(&boot, WaveLifecycle::Working).await;

    let err = request_ratification(&boot, "cap_exhausted retry")
        .await
        .expect_err("pending request must reject duplicate");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(
        err.message.contains("not in `working`")
            && err.message.contains("ratify request is already pending"),
        "{err:?}"
    );

    let events = events_for_wave(&boot, &["ratify.requested"]).await;
    assert_eq!(
        events.len(),
        1,
        "duplicate pending request must not append: {events:?}"
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
async fn ratify_route_rejects_non_pending_wave_for_all_decisions_without_event() {
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
async fn ratify_route_rejects_blocked_wave_without_pending_request_or_event() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Blocked).await;

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
            "blocked-without-pending {decision} must not append: {events:?}"
        );
    }
}

#[tokio::test]
async fn ratify_route_rejects_stale_second_verdict_after_grant_without_second_event() {
    let boot = boot().await;
    request_ratification(&boot, "cap_exhausted")
        .await
        .expect("ratify request");

    let (status, body) = post_ratify(&boot, "grant").await;
    assert_eq!(status, StatusCode::OK, "{body}");

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
        assert_eq!(
            events.len(),
            1,
            "stale {decision} must not append a second resolution: {events:?}"
        );
    }
}

#[tokio::test]
async fn ratify_route_grant_emits_resolved_and_flips_blocked_to_working() {
    let boot = boot().await;
    request_ratification(&boot, "cap_exhausted")
        .await
        .expect("ratify request");

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

// ===================== #888 post-grant cap extension =====================
//
// The kernel accepts a per-subject cap raise only immediately after genuine
// exhaustion (prev n == prev cap), only when backed by a `ratify.resolved
// { grant }` strictly newer than the exhausting round, and only by exactly
// CAP_EXTENSION_PER_GRANT = 2 (INV-CAP-EXT, #888 design §3).

/// `valid_round_args` variant with explicit n/cap/converged on the standard
/// impl subject. Non-converged rounds carry changes_requested verdicts
/// (converged=true requires all-approved).
fn round_args(n: u32, cap: u32, converged: bool) -> Value {
    round_args_for_subject(
        json!({ "phase": "impl", "slice_id": "5b", "pr_number": 760 }),
        n,
        cap,
        converged,
    )
}

fn round_args_for_subject(subject: Value, n: u32, cap: u32, converged: bool) -> Value {
    let verdict = if converged {
        "approved"
    } else {
        "changes_requested"
    };
    json!({
        "subject": subject,
        "head_sha": "abc123",
        "n": n,
        "cap": cap,
        "converged": converged,
        "channels": [
            { "role": "reviewer-a", "verdict": verdict },
            { "role": "reviewer-b", "verdict": verdict }
        ]
    })
}

async fn emit_round(boot: &Boot, args: Value) {
    let out = call_tool(boot, TOOL_REVIEW_ROUND, args)
        .await
        .expect("review round accepted");
    assert_eq!(out["emitted"], json!(true), "{out}");
}

/// Drive the standard impl subject to exhaustion: rounds n=1..=cap at `cap`,
/// all non-converged.
async fn exhaust_subject(boot: &Boot, cap: u32) {
    for n in 1..=cap {
        emit_round(boot, round_args(n, cap, false)).await;
    }
}

/// Reject helper: INVALID_PARAMS + message fragment + no event appended.
async fn expect_round_reject(boot: &Boot, args: Value, fragment: &str) {
    let before = events_for_wave(boot, &["review.round"]).await.len();
    let err = call_tool(boot, TOOL_REVIEW_ROUND, args)
        .await
        .expect_err("review round must be rejected");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(err.message.contains(fragment), "{err:?}");
    let after = events_for_wave(boot, &["review.round"]).await.len();
    assert_eq!(after, before, "rejected round must not append");
}

/// working -> blocked (request) -> working (grant).
async fn request_and_grant(boot: &Boot, reason: &str) {
    request_ratification(boot, reason)
        .await
        .expect("ratify request");
    let (status, body) = post_ratify(boot, "grant").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

// t1 — no grant at all: exhaustion alone does not authorize an extension.
#[tokio::test]
async fn review_round_cap_extension_without_grant_rejected() {
    let boot = boot().await;
    exhaust_subject(&boot, 3).await;
    expect_round_reject(
        &boot,
        round_args(4, 5, false),
        "requires a ratify.resolved grant",
    )
    .await;
}

// t2 — fresh grant: exactly-+2 extension accepted, in-window continuation
// accepted, extended window re-exhausts at the static guard.
#[tokio::test]
async fn review_round_cap_extension_with_fresh_grant_accepted() {
    let boot = boot().await;
    exhaust_subject(&boot, 3).await;
    request_and_grant(&boot, "cap_exhausted").await;

    emit_round(&boot, round_args(4, 5, false)).await;
    // In-window continuation after an extension (unit-only coverage; the R7c
    // E2E's single post-grant round both extends and converges).
    emit_round(&boot, round_args(5, 5, false)).await;
    // The extended window re-exhausts: n=6 > cap=5 hits the static guard.
    expect_round_reject(&boot, round_args(6, 5, false), "must be <=").await;

    let events = events_for_wave(&boot, &["review.round"]).await;
    assert_eq!(events.len(), 5, "{events:?}");
}

// t3 — the non-reuse theorem, executable form: the first grant's row id
// precedes the new exhausting round, so a second extension needs a FRESH
// grant.
#[tokio::test]
async fn review_round_second_extension_requires_fresh_grant() {
    let boot = boot().await;
    exhaust_subject(&boot, 3).await;
    request_and_grant(&boot, "cap_exhausted").await;
    emit_round(&boot, round_args(4, 5, false)).await;
    emit_round(&boot, round_args(5, 5, false)).await;

    // The old grant is now STALE: its row id < id(round n=5), the new
    // exhausting round.
    expect_round_reject(
        &boot,
        round_args(6, 7, false),
        "requires a ratify.resolved grant",
    )
    .await;

    request_and_grant(&boot, "cap_exhausted again").await;
    emit_round(&boot, round_args(6, 7, false)).await;
}

// t4 — cap must never shrink.
#[tokio::test]
async fn review_round_cap_shrink_rejected() {
    let boot = boot().await;
    emit_round(&boot, round_args(1, 3, false)).await;
    expect_round_reject(&boot, round_args(2, 2, false), "must not shrink").await;
}

// t5 — a deny is not a grant.
#[tokio::test]
async fn review_round_deny_does_not_authorize_extension() {
    let boot = boot().await;
    exhaust_subject(&boot, 3).await;
    request_ratification(&boot, "cap_exhausted")
        .await
        .expect("ratify request");
    let (status, body) = post_ratify(&boot, "deny").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The wave stays blocked, but calm.review.round has no lifecycle guard —
    // the reject below is the CAP arm's doing, not a lifecycle side effect.
    expect_round_reject(
        &boot,
        round_args(4, 5, false),
        "requires a ratify.resolved grant",
    )
    .await;
}

// t5b — a later deny does NOT revoke an earlier grant's extension
// authorization (#888 design §3.7 decided semantics; also pins the cap arm's
// lifecycle-independence: the wave is `blocked` when the extension lands).
#[tokio::test]
async fn review_round_deny_after_grant_does_not_revoke_extension() {
    let boot = boot().await;
    exhaust_subject(&boot, 3).await;
    request_and_grant(&boot, "cap_exhausted").await;
    // Grant restored `working`, so a second (free-form) request is legal.
    request_ratification(&boot, "second thoughts")
        .await
        .expect("second ratify request");
    let (status, body) = post_ratify(&boot, "deny").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let out = call_tool(&boot, TOOL_REVIEW_ROUND, round_args(4, 5, false))
        .await
        .expect("extension backed by the earlier grant");
    assert_eq!(out["emitted"], json!(true), "{out}");
}

// t6 — a grant does not license mid-stream inflation: the previous window
// must be exhausted first.
#[tokio::test]
async fn review_round_extension_before_exhaustion_rejected() {
    let boot = boot().await;
    emit_round(&boot, round_args(1, 3, false)).await;
    emit_round(&boot, round_args(2, 3, false)).await;
    request_and_grant(&boot, "early ask").await;
    expect_round_reject(
        &boot,
        round_args(3, 5, false),
        "requires the previous window to be exhausted",
    )
    .await;
}

// t7 — the delta is exactly +2: over- and under-raise both rejected.
#[tokio::test]
async fn review_round_extension_wrong_delta_rejected() {
    let boot = boot().await;
    exhaust_subject(&boot, 3).await;
    request_and_grant(&boot, "cap_exhausted").await;
    expect_round_reject(&boot, round_args(4, 6, false), "exactly").await;
    expect_round_reject(&boot, round_args(4, 4, false), "exactly").await;
    // Sanity tail: the exact +2 raise is accepted.
    emit_round(&boot, round_args(4, 5, false)).await;
}

// t8 — byte-identical crash-retry of an extension round is a no-op
// (DuplicateSame precedes the cap arm; grant freshness is not re-litigated).
#[tokio::test]
async fn review_round_extension_duplicate_resubmit_noop() {
    let boot = boot().await;
    exhaust_subject(&boot, 3).await;
    request_and_grant(&boot, "cap_exhausted").await;
    emit_round(&boot, round_args(4, 5, false)).await;

    let before = events_for_wave(&boot, &["review.round"]).await.len();
    let out = call_tool(&boot, TOOL_REVIEW_ROUND, round_args(4, 5, false))
        .await
        .expect("byte-identical resubmit is idempotent");
    assert_eq!(out["emitted"], json!(false), "{out}");
    let after = events_for_wave(&boot, &["review.round"]).await.len();
    assert_eq!(after, before, "duplicate must not append");
}

// t8b — DuplicateSame equality is byte-identical and order-sensitive
// (pre-existing semantics, honestly pinned): a resubmit with the channel
// verdicts swapped is NOT a duplicate and fails the n check.
#[tokio::test]
async fn review_round_extension_resubmit_reordered_channels_rejected() {
    let boot = boot().await;
    exhaust_subject(&boot, 3).await;
    request_and_grant(&boot, "cap_exhausted").await;
    emit_round(&boot, round_args(4, 5, false)).await;

    let mut reordered = round_args(4, 5, false);
    let channels = reordered["channels"]
        .as_array_mut()
        .expect("channels array");
    channels.swap(0, 1);
    expect_round_reject(&boot, reordered, "expected n=5").await;
}

// t9 — Q1 breadth (#888 design §3.6): ONE grant authorizes one +2 extension
// for EVERY subject of the wave that was already exhausted when it landed.
#[tokio::test]
async fn review_round_one_grant_extends_each_subject_exhausted_before_it() {
    let boot = boot().await;
    let impl_subject = json!({ "phase": "impl", "slice_id": "5b", "pr_number": 760 });
    let design_subject = json!({ "phase": "design", "slice_id": "5b" });
    for n in 1..=3 {
        emit_round(
            &boot,
            round_args_for_subject(impl_subject.clone(), n, 3, false),
        )
        .await;
        emit_round(
            &boot,
            round_args_for_subject(design_subject.clone(), n, 3, false),
        )
        .await;
    }
    request_and_grant(&boot, "cap_exhausted").await;

    emit_round(&boot, round_args_for_subject(impl_subject, 4, 5, false)).await;
    emit_round(&boot, round_args_for_subject(design_subject, 4, 5, false)).await;
}

/// Seed a `review.round` row for the standard impl subject directly via
/// `log_pure_event`, bypassing the tool's kernel (events carry no idempotency
/// uniqueness index; role_gate 2.8 admits AiSpec(spec card)). Used for
/// histories the tool cannot produce: tied-n rows (t10) and u32-boundary
/// n/cap values (t12/t13 — the only way there without ~4B tool calls).
async fn seed_pure_round(boot: &Boot, n: u32, cap: u32, tag: &str) {
    boot.repo
        .log_pure_event(
            ActorId::AiSpec(boot.spec_card_id.clone()),
            EventScope::Wave {
                wave: boot.wave_id.clone(),
                cove: boot.cove_id.clone(),
            },
            None,
            &boot.events,
            &boot.card_role_cache,
            &boot.wave_cove_cache,
            Event::ReviewRound {
                wave_id: boot.wave_id.clone(),
                subject: ReviewSubject {
                    phase: "impl".into(),
                    slice_id: "5b".into(),
                    pr_number: Some(760),
                },
                head_sha: Some(format!("seed-{tag}")),
                n,
                cap,
                converged: false,
                channels: vec![
                    ChannelVerdict {
                        role: "reviewer-a".into(),
                        verdict: ChannelVerdictKind::ChangesRequested,
                    },
                    ChannelVerdict {
                        role: "reviewer-b".into(),
                        verdict: ChannelVerdictKind::ChangesRequested,
                    },
                ],
                root_cause: None,
                idempotency_key: format!("review.round:{}:impl:5b:760:{n}:{tag}", boot.wave_id),
            },
        )
        .await
        .expect("seed review.round");
}

// t10 — tied-n recovery pick (#888 design §3.1): among tied max-n rows,
// `prev` is the one with the greatest event row id. Ties cannot arise through
// the tool; seed one via `log_pure_event`.
#[tokio::test]
async fn review_round_tied_n_prev_pick_is_greatest_row_id() {
    let boot = boot().await;
    exhaust_subject(&boot, 3).await;

    // Seed a SECOND n=3 row with cap=5 (distinct idem-key suffix): the
    // greatest-row-id tied row now says cap=5, n=3 (NOT exhausted for cap 5).
    seed_pure_round(&boot, 3, 5, "tied").await;

    // n=4/cap=5 with NO grant: accepted iff `prev` is the seeded greatest-
    // row-id row (equal cap, rule row 4). Picking the older tied row
    // (n=3=cap=3, exhausted) would instead demand a grant (E3).
    emit_round(&boot, round_args(4, 5, false)).await;
}

// t11 — rule-table order, discriminating form: at prev n=3=cap=3 with a
// fresh grant, submit n=5/cap=6. cap=6 is a cap the cap arm would
// independently E4-reject ("must raise cap by exactly 2": the legal
// extension cap is 5), and n=5 is wrong (expected n=4) — so which message
// surfaces discriminates the ordering. Asserting the n-message AND the
// absence of the E4 "exactly" message proves rows 3 (n check) fire before
// rows 4-9 (cap arm), preserving the existing re-sync signal. (A n=5/cap=5
// probe would be vacuous here: cap=5 is the LEGAL extension cap, so both
// orderings report "expected n=4".)
#[tokio::test]
async fn review_round_wrong_n_and_cap_reports_n_error() {
    let boot = boot().await;
    exhaust_subject(&boot, 3).await;
    request_and_grant(&boot, "cap_exhausted").await;

    let before = events_for_wave(&boot, &["review.round"]).await.len();
    let err = call_tool(&boot, TOOL_REVIEW_ROUND, round_args(5, 6, false))
        .await
        .expect_err("wrong-n + wrong-cap round must be rejected");
    assert_eq!(
        err.code,
        calm_server::plugin_host::mcp::RpcError::INVALID_PARAMS
    );
    assert!(err.message.contains("expected n=4"), "{err:?}");
    assert!(
        !err.message.contains("exactly"),
        "n check must fire before the cap arm's E4 reject: {err:?}"
    );
    let after = events_for_wave(&boot, &["review.round"]).await.len();
    assert_eq!(after, before, "rejected round must not append");
}

// t12 — u32 boundary on expected-n (INV-CAP-EXT): once a subject's numbering
// reaches n=u32::MAX, NO further round is accepted — a saturated expected-n
// would re-admit further distinct rows at the same n. Seeded via
// `log_pure_event` (the only way to reach the boundary without ~4B tool
// calls). The distinct-payload resubmit at n=u32::MAX is exactly the row a
// saturating expected-n would have accepted; zero events appended.
#[tokio::test]
async fn review_round_n_at_u32_max_rejects_further_rounds() {
    let boot = boot().await;
    seed_pure_round(&boot, u32::MAX, u32::MAX, "nmax").await;
    expect_round_reject(
        &boot,
        round_args(u32::MAX, u32::MAX, false),
        "round numbering exhausted",
    )
    .await;
}

// t13 — u32 boundary on the extension cap (INV-CAP-EXT): with prev
// n=cap=u32::MAX-1 exhausted and a FRESH grant, the exactly-+2 target does
// not exist in u32 — the raise to u32::MAX (what a saturating expected-cap
// would have "expected" and accepted) is a +1 extension and must be
// rejected; zero events appended.
#[tokio::test]
async fn review_round_cap_extension_at_u32_boundary_rejected() {
    let boot = boot().await;
    seed_pure_round(&boot, u32::MAX - 1, u32::MAX - 1, "capmax").await;
    request_and_grant(&boot, "cap_exhausted at u32 boundary").await;
    expect_round_reject(
        &boot,
        round_args(u32::MAX, u32::MAX, false),
        "cap extension space exhausted",
    )
    .await;
}

#[tokio::test]
async fn ratify_route_deny_emits_resolved_and_stays_blocked() {
    let boot = boot().await;
    request_ratification(&boot, "cap_exhausted")
        .await
        .expect("ratify request");

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
