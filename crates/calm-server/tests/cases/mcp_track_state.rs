//! PR7b (#136) — `mcp_server::tools::track_state` integration smoke.
//!
//! Boots an in-memory `SqlxRepo` + an `EventBus` + a pre-seeded
//! `CardRoleCache`, constructs an `AppContext` directly (no live MCP
//! listener — these tests exercise the handlers as plain async fns),
//! and asserts the end-to-end happy paths for each tool.
//!
//! Coverage:
//!
//!   1. `calm.track.state` (planner card) returns the track row + the cards
//!      list with `role` populated.
//!   2. `calm.task.verdict` with `status=accepted` emits
//!      `task.completed` carrying the planner's `{status,reason}` verdict
//!      in `result`; `status=rejected` emits `task.failed` with the
//!      reason verbatim.
//!
//! No live UDS, no handshake — the track-state tools' contract is
//! "given a `ToolCallIdentity` + `Value` args, do the right thing"; the
//! transport layer's job is to bind the identity, and that's
//! exercised by PR7a's handshake tests + the PR7a.1 worker MCP wiring
//! tests.

use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::RepoEventWrite;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_insert_tx, session_mark_track_root_tx};
use calm_server::error::CalmError;
use calm_server::event::{Event, EventBus};
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::track_state::{TOOL_TASK_VERDICT, TOOL_TRACK_STATE};
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{
    CardRole, CardRuntimeView, NewArea, NewCard, NewTrack, TrackLifecycle, TrackPatch,
};
use calm_server::plugin_host::mcp::RpcError;
use calm_server::session_projection_repo::AgentProvider;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use serde_json::{Value, json};

const PLANNER_SESSION_ID: &str = "planner-session";

/// One-shot boot: in-memory sqlite + bus + cache + one area with one
/// track with one planner card and one worker card. Returns enough handles
/// to drive a tool through its registered closure.
struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    repo: Arc<dyn Repo>,
    area_id: AreaId,
    track_id: TrackId,
    planner_card_id: CardId,
    worker_card_id: CardId,
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
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "mcp-track-state".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "initial".into(),
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
            kind: "planner".into(),
            sort: None,
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap();
    let worker_card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
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
    // Manually pin the roles. The tx-suffixed mint helpers do the cache
    // write-through in production; for this test, we mock the post-mint
    // state.
    card_role_cache.insert(planner_card.id.clone(), CardRole::Planner, track.id.clone());
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, track.id.clone());
    // #1189 §3.6 — the persisted column is no longer optional. `enforce_role`
    // still only reads the cache, but the recorder gate resolves
    // session → card → {role, track} with a live `cards` read inside the write
    // tx, so a cache-only pin would leave this "planner card" persisted as a
    // worker and deny every write for a reason no test here is about.
    crate::support::mcp::set_persisted_card_role(
        repo.as_ref(),
        planner_card.id.as_str(),
        CardRole::Planner,
    )
    .await;
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        track_vcs: repo
            .sqlite_pool()
            .map(calm_truth::track_vcs_repo::SqlxTrackVcsRepo::shared),
        events,
        write: calm_server::state::WriteContext::new(card_role_cache, track_area_cache),
        daemon_token_hash: None,
        gate_logs_dir: std::env::temp_dir().join("neige-test-gate-logs"),
        task_budget_default: calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
        plugin_host: Arc::new(tokio::sync::OnceCell::new()),
        operation_runtime: Arc::new(tokio::sync::OnceCell::new()),
    });

    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    Boot {
        ctx,
        registry,
        repo,
        area_id: area.id,
        track_id: track.id,
        planner_card_id: planner_card.id,
        worker_card_id: worker_card.id,
    }
}

/// Drive a tool via the registry the way the transport does — by
/// looking it up and invoking the boxed handler. Returns the tool's
/// `Result<Value, RpcError>` (the RpcError's `Display` is opaque, so
/// the caller inspects `.code` / `.message` directly).
async fn call_tool(
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

fn worker_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.worker_card_id.as_str().to_string(),
        role: CardRole::Worker,
        provider: AgentProvider::Codex,
        session_id: "worker-session".to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "worker-thread".to_string(),
    }
}

async fn set_track_lifecycle(boot: &Boot, lifecycle: TrackLifecycle) {
    boot.repo
        .track_update(
            boot.track_id.as_str(),
            TrackPatch {
                lifecycle: Some(lifecycle),
                ..Default::default()
            },
        )
        .await
        .expect("set test track lifecycle");
}

// ---------------------------------------------------------------------------
// calm.track.state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_track_state_returns_track_and_cards_for_planner() {
    let boot = boot().await;
    let out = call_tool(&boot, TOOL_TRACK_STATE, planner_identity(&boot), json!({}))
        .await
        .expect("planner can read track state");

    let track = out.get("track").expect("response carries `track`");
    assert_eq!(
        track.get("id").and_then(Value::as_str),
        Some(boot.track_id.as_str()),
        "track.id matches the bound planner card's track",
    );
    assert_eq!(
        track.get("title").and_then(Value::as_str),
        Some("initial"),
        "track.title matches the boot fixture",
    );

    let cards = out
        .get("cards")
        .and_then(Value::as_array)
        .expect("response carries `cards`");
    assert_eq!(cards.len(), 2, "boot fixture mints exactly two cards");

    // Find the planner card in the list and assert its role.
    let planner = cards
        .iter()
        .find(|c| c.get("id").and_then(Value::as_str) == Some(boot.planner_card_id.as_str()))
        .expect("planner card present");
    assert_eq!(planner.get("role").and_then(Value::as_str), Some("planner"));
    assert!(
        planner.get("runtime").is_some(),
        "planner card = {planner:?}"
    );
    let planner_runtime: Option<CardRuntimeView> =
        serde_json::from_value(planner["runtime"].clone()).expect("runtime field is typed");
    assert!(planner_runtime.is_none(), "planner card has no runtime row");

    let worker = cards
        .iter()
        .find(|c| c.get("id").and_then(Value::as_str) == Some(boot.worker_card_id.as_str()))
        .expect("worker card present");
    assert_eq!(worker.get("role").and_then(Value::as_str), Some("worker"));
    assert!(worker.get("runtime").is_some(), "worker card = {worker:?}");
    let worker_runtime: Option<CardRuntimeView> =
        serde_json::from_value(worker["runtime"].clone()).expect("runtime field is typed");
    assert!(worker_runtime.is_none(), "worker card has no runtime row");
    assert_eq!(
        out.get("report_startup_read_required")
            .and_then(Value::as_bool),
        Some(false),
        "a track with no report card is not a forked plan"
    );
}

#[tokio::test]
async fn get_track_state_callable_by_worker() {
    // Confirms the planner-only soft role gate doesn't fire on read.
    let boot = boot().await;
    let out = call_tool(&boot, TOOL_TRACK_STATE, worker_identity(&boot), json!({}))
        .await
        .expect("worker can also read track state — no role gate on read");
    assert_eq!(
        out.get("track")
            .and_then(|w| w.get("id"))
            .and_then(Value::as_str),
        Some(boot.track_id.as_str()),
    );
}

// ---------------------------------------------------------------------------
// calm.task.verdict
// ---------------------------------------------------------------------------

#[tokio::test]
async fn task_verdict_accepted_emits_task_completed() {
    let boot = boot().await;
    let mut rx = boot.ctx.events.subscribe();

    let out = call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        planner_identity(&boot),
        json!({
            "idempotency_key": "job-xyz",
            "status": "accepted",
            "reason": "looks great",
            "message": "accept worker result"
        }),
    )
    .await
    .expect("planner accept verdict ok");
    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));

    let auto_changed = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers auto lifecycle")
        .expect("bus open");
    assert!(matches!(auto_changed.actor, ActorId::Kernel));
    assert!(matches!(
        auto_changed.event,
        Event::TrackLifecycleChanged {
            from: TrackLifecycle::Draft,
            to: TrackLifecycle::Planning,
            agent_message: Some(ref message),
            ..
        } if message == "[auto] first planner write"
    ));
    let auto_updated = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers auto update")
        .expect("bus open");
    assert!(matches!(auto_updated.actor, ActorId::Kernel));
    assert!(matches!(
        auto_updated.event,
        Event::TrackUpdated(ref payload)
            if payload.agent_message.as_deref() == Some("[auto] first planner write")
    ));
    let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    let (idem, result) = match envelope.event {
        Event::TaskCompleted {
            idempotency_key,
            result,
            ..
        } => (idempotency_key, result),
        other => panic!("expected TaskCompleted, got {other:?}"),
    };
    assert_eq!(idem, "job-xyz");
    assert_eq!(
        result.get("status").and_then(Value::as_str),
        Some("accepted")
    );
    assert_eq!(
        result.get("reason").and_then(Value::as_str),
        Some("looks great"),
        "planner's rationale is folded into `result`",
    );
}

#[tokio::test]
async fn legacy_alias_update_task_meta_still_dispatches_via_warn() {
    let boot = boot().await;
    set_track_lifecycle(&boot, TrackLifecycle::Planning).await;
    let mut rx = boot.ctx.events.subscribe();

    let out = call_tool(
        &boot,
        "calm.update_task_meta",
        planner_identity(&boot),
        json!({
            "idempotency_key": "legacy-job",
            "status": "accepted",
            "reason": "legacy alias forwards",
            "message": "legacy alias forwards"
        }),
    )
    .await
    .expect("legacy alias forwards to task verdict");
    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));

    let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    match envelope.event {
        Event::TaskCompleted {
            idempotency_key,
            result,
            ..
        } => {
            assert_eq!(idempotency_key, "legacy-job");
            assert_eq!(
                result.get("status").and_then(Value::as_str),
                Some("accepted")
            );
        }
        other => panic!("expected TaskCompleted, got {other:?}"),
    }
}

#[tokio::test]
async fn task_verdict_rejected_emits_task_failed() {
    let boot = boot().await;
    let mut rx = boot.ctx.events.subscribe();

    let out = call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        planner_identity(&boot),
        json!({
            "idempotency_key": "job-xyz",
            "status": "rejected",
            "reason": "missed acceptance criterion #3",
            "message": "reject worker result"
        }),
    )
    .await
    .expect("planner reject verdict ok");
    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));

    let auto_changed = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers auto lifecycle")
        .expect("bus open");
    assert!(matches!(auto_changed.actor, ActorId::Kernel));
    assert!(matches!(
        auto_changed.event,
        Event::TrackLifecycleChanged {
            from: TrackLifecycle::Draft,
            to: TrackLifecycle::Planning,
            agent_message: Some(ref message),
            ..
        } if message == "[auto] first planner write"
    ));
    let auto_updated = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers auto update")
        .expect("bus open");
    assert!(matches!(auto_updated.actor, ActorId::Kernel));
    assert!(matches!(
        auto_updated.event,
        Event::TrackUpdated(ref payload)
            if payload.agent_message.as_deref() == Some("[auto] first planner write")
    ));
    let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    match envelope.event {
        Event::TaskFailed {
            idempotency_key,
            reason,
            ..
        } => {
            assert_eq!(idempotency_key, "job-xyz");
            assert_eq!(reason, "missed acceptance criterion #3");
        }
        other => panic!("expected TaskFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn task_verdict_unknown_status_rejected() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        planner_identity(&boot),
        json!({
            "idempotency_key": "k",
            "status": "maybe",
            "message": "bad status",
        }),
    )
    .await
    .expect_err("unknown status rejected");
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("maybe"), "echoes the bad status");
}

#[tokio::test]
async fn task_verdict_worker_refused_at_mcp_entry() {
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        worker_identity(&boot),
        json!({
            "idempotency_key": "k",
            "status": "accepted",
        }),
    )
    .await
    .expect_err("worker can't record a planner verdict");
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("Planner"));
}

#[tokio::test]
async fn task_verdict_requires_non_empty_message() {
    let boot = boot().await;

    let err = call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        planner_identity(&boot),
        json!({
            "idempotency_key": "missing-message",
            "status": "accepted"
        }),
    )
    .await
    .expect_err("missing message rejected");
    assert_eq!(err.code, -32602);
    assert!(
        err.message.contains("message must be non-empty"),
        "msg = {err:?}"
    );

    let err = call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        planner_identity(&boot),
        json!({
            "idempotency_key": "empty-message",
            "status": "accepted",
            "message": "\t \n"
        }),
    )
    .await
    .expect_err("empty message rejected");
    assert_eq!(err.code, -32602);
    assert!(
        err.message.contains("message must be non-empty"),
        "msg = {err:?}"
    );
}

#[tokio::test]
async fn task_verdict_without_lifecycle_keeps_track_state_and_records_message() {
    let boot = boot().await;
    set_track_lifecycle(&boot, TrackLifecycle::Planning).await;
    let mut rx = boot.ctx.events.subscribe();

    call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        planner_identity(&boot),
        json!({
            "idempotency_key": "verdict-no-lifecycle",
            "status": "accepted",
            "reason": "ok",
            "message": "accept without lifecycle"
        }),
    )
    .await
    .expect("verdict succeeds");

    let env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers verdict")
        .expect("bus open");
    match env.event {
        Event::TaskCompleted {
            idempotency_key,
            agent_message,
            ..
        } => {
            assert_eq!(idempotency_key, "verdict-no-lifecycle");
            assert_eq!(agent_message.as_deref(), Some("accept without lifecycle"));
        }
        other => panic!("expected TaskCompleted, got {other:?}"),
    }
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track.lifecycle, TrackLifecycle::Planning);
    let no_more = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(no_more.is_err(), "unexpected lifecycle event: {no_more:?}");
}

#[tokio::test]
async fn task_verdict_lifecycle_legal_emits_track_updated_and_verdict() {
    let boot = boot().await;
    set_track_lifecycle(&boot, TrackLifecycle::Planning).await;
    let mut rx = boot.ctx.events.subscribe();

    call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        planner_identity(&boot),
        json!({
            "idempotency_key": "verdict-legal-lifecycle",
            "status": "accepted",
            "reason": "ok",
            "message": "accept and dispatch",
            "lifecycle": "dispatching"
        }),
    )
    .await
    .expect("verdict with lifecycle succeeds");

    let changed_env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers lifecycle")
        .expect("bus open");
    match changed_env.event {
        Event::TrackLifecycleChanged {
            id,
            area_id,
            from,
            to,
            agent_message,
        } => {
            assert_eq!(id, boot.track_id);
            assert_eq!(area_id, boot.area_id);
            assert_eq!(from, TrackLifecycle::Planning);
            assert_eq!(to, TrackLifecycle::Dispatching);
            assert_eq!(agent_message.as_deref(), Some("accept and dispatch"));
        }
        other => panic!("expected TrackLifecycleChanged first, got {other:?}"),
    }
    let updated_env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers track update")
        .expect("bus open");
    match updated_env.event {
        Event::TrackUpdated(payload) => {
            assert_eq!(payload.id, boot.track_id);
            assert_eq!(payload.lifecycle, TrackLifecycle::Dispatching);
            assert_eq!(
                payload.agent_message.as_deref(),
                Some("accept and dispatch")
            );
        }
        other => panic!("expected TrackUpdated second, got {other:?}"),
    }
    let verdict_env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers verdict")
        .expect("bus open");
    match verdict_env.event {
        Event::TaskCompleted {
            idempotency_key,
            agent_message,
            ..
        } => {
            assert_eq!(idempotency_key, "verdict-legal-lifecycle");
            assert_eq!(agent_message.as_deref(), Some("accept and dispatch"));
        }
        other => panic!("expected TaskCompleted third, got {other:?}"),
    }
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track.lifecycle, TrackLifecycle::Dispatching);
}

#[tokio::test]
async fn task_verdict_lifecycle_illegal_rolls_back_verdict_and_events() {
    let boot = boot().await;
    set_track_lifecycle(&boot, TrackLifecycle::Planning).await;
    let mut rx = boot.ctx.events.subscribe();

    let err = call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        planner_identity(&boot),
        json!({
            "idempotency_key": "verdict-illegal-lifecycle",
            "status": "accepted",
            "reason": "ok",
            "message": "illegal verdict lifecycle",
            "lifecycle": "done"
        }),
    )
    .await
    .expect_err("planning -> done is illegal");
    assert_eq!(err.code, -32403);

    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track.lifecycle, TrackLifecycle::Planning);
    let no_event = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_event.is_err(),
        "illegal transition emitted event: {no_event:?}"
    );

    let events = boot.repo.events_since(0, 100).await.unwrap();
    assert!(
        events.iter().all(
            |(_, _, _, event)| !matches!(event, Event::TaskCompleted { idempotency_key, .. }
                if idempotency_key == "verdict-illegal-lifecycle")
        ),
        "rolled-back verdict must not be persisted: {events:?}"
    );
}

// ---------------------------------------------------------------------------
// Track lifecycle defaults
// ---------------------------------------------------------------------------

#[tokio::test]
async fn new_track_defaults_to_draft_lifecycle() {
    let boot = boot().await;
    let track = boot
        .repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        track.lifecycle,
        calm_server::model::TrackLifecycle::Draft,
        "freshly minted track starts in Draft"
    );
}
