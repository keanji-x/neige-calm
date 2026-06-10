//! PR7b (#136) — `mcp_server::tools::wave_state` integration smoke.
//!
//! Boots an in-memory `SqlxRepo` + an `EventBus` + a pre-seeded
//! `CardRoleCache`, constructs an `AppContext` directly (no live MCP
//! listener — these tests exercise the handlers as plain async fns),
//! and asserts the end-to-end happy paths for each tool.
//!
//! Coverage:
//!
//!   1. `calm.wave.state` (spec card) returns the wave row + the cards
//!      list with `role` populated.
//!   2. `update_wave_state` from a spec card patches the wave row,
//!      stamps `updated_at`, and emits exactly one `wave.updated`
//!      event on the bus + in the persisted events log.
//!   3. `update_wave_state` from a worker card is refused at the MCP
//!      entry with `-32602` (soft role gate) **before** any DB write
//!      runs.
//!   4. `calm.task.verdict` with `status=accepted` emits
//!      `task.completed` carrying the spec's `{status,reason}` verdict
//!      in `result`; `status=rejected` emits `task.failed` with the
//!      reason verbatim.
//!
//! No live UDS, no handshake — the wave-state tools' contract is
//! "given a `ToolCallIdentity` + `Value` args, do the right thing"; the
//! transport layer's job is to bind the identity, and that's
//! exercised by PR7a's handshake tests + the PR7a.1 worker MCP wiring
//! tests.

use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, CoveId, WaveId};
use calm_server::mcp_server::registry::AppContext;
use calm_server::mcp_server::tools::wave_state::{
    TOOL_TASK_VERDICT, TOOL_UPDATE_WAVE_STATE, TOOL_WAVE_STATE,
};
use calm_server::mcp_server::{ToolCallIdentity, ToolRegistry};
use calm_server::model::{
    CardRole, CardRuntimeView, NewCard, NewCove, NewWave, WaveLifecycle, WavePatch,
};
use calm_server::plugin_host::mcp::RpcError;
use serde_json::{Value, json};

/// One-shot boot: in-memory sqlite + bus + cache + one cove with one
/// wave with one spec card and one worker card. Returns enough handles
/// to drive a tool through its registered closure.
struct Boot {
    ctx: Arc<AppContext>,
    registry: Arc<ToolRegistry>,
    repo: Arc<dyn Repo>,
    cove_id: CoveId,
    wave_id: WaveId,
    spec_card_id: CardId,
    worker_card_id: CardId,
}

async fn boot() -> Boot {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "mcp-wave-state".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "initial".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let spec_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap();
    let worker_card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: serde_json::Value::Null,
        })
        .await
        .unwrap();

    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    // Manually pin the roles. The tx-suffixed mint helpers do the cache
    // write-through in production; for this test, we mock the post-mint
    // state.
    card_role_cache.insert(spec_card.id.clone(), CardRole::Spec, wave.id.clone());
    card_role_cache.insert(worker_card.id.clone(), CardRole::Worker, wave.id.clone());

    // Bypass the persisted-role update — `enforce_role` only reads
    // the cache, so a cache-only pin is sufficient to drive the gate.
    // (The full path runs `card_with_codex_create_tx` which writes
    // both the row and the cache; PR7b's integration test doesn't
    // need to assert on the persisted column.)
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let ctx = Arc::new(AppContext {
        repo: route_repo,
        events,
        write: calm_server::state::WriteContext::new(card_role_cache, wave_cove_cache),
        daemon_token_hash: None,
    });

    let mut registry = ToolRegistry::new();
    calm_server::mcp_server::tools::register_default_tools(&mut registry);
    let registry = Arc::new(registry);

    Boot {
        ctx,
        registry,
        repo,
        cove_id: cove.id,
        wave_id: wave.id,
        spec_card_id: spec_card.id,
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

fn spec_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.spec_card_id.as_str().to_string(),
        role: CardRole::Spec,
        wave_id: Some(boot.wave_id.as_str().to_string()),
        thread_id: "spec-thread".to_string(),
    }
}

fn worker_identity(boot: &Boot) -> ToolCallIdentity {
    ToolCallIdentity {
        card_id: boot.worker_card_id.as_str().to_string(),
        role: CardRole::Worker,
        wave_id: Some(boot.wave_id.as_str().to_string()),
        thread_id: "worker-thread".to_string(),
    }
}

async fn set_wave_lifecycle(boot: &Boot, lifecycle: WaveLifecycle) {
    boot.repo
        .wave_update(
            boot.wave_id.as_str(),
            WavePatch {
                lifecycle: Some(lifecycle),
                ..Default::default()
            },
        )
        .await
        .expect("set test wave lifecycle");
}

// ---------------------------------------------------------------------------
// calm.wave.state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_wave_state_returns_wave_and_cards_for_spec() {
    let boot = boot().await;
    let out = call_tool(&boot, TOOL_WAVE_STATE, spec_identity(&boot), json!({}))
        .await
        .expect("spec can read wave state");

    let wave = out.get("wave").expect("response carries `wave`");
    assert_eq!(
        wave.get("id").and_then(Value::as_str),
        Some(boot.wave_id.as_str()),
        "wave.id matches the bound spec card's wave",
    );
    assert_eq!(
        wave.get("title").and_then(Value::as_str),
        Some("initial"),
        "wave.title matches the boot fixture",
    );

    let cards = out
        .get("cards")
        .and_then(Value::as_array)
        .expect("response carries `cards`");
    assert_eq!(cards.len(), 2, "boot fixture mints exactly two cards");

    // Find the spec card in the list and assert its role.
    let spec = cards
        .iter()
        .find(|c| c.get("id").and_then(Value::as_str) == Some(boot.spec_card_id.as_str()))
        .expect("spec card present");
    assert_eq!(spec.get("role").and_then(Value::as_str), Some("spec"));
    assert!(spec.get("runtime").is_some(), "spec card = {spec:?}");
    let spec_runtime: Option<CardRuntimeView> =
        serde_json::from_value(spec["runtime"].clone()).expect("runtime field is typed");
    assert!(spec_runtime.is_none(), "spec card has no runtime row");

    let worker = cards
        .iter()
        .find(|c| c.get("id").and_then(Value::as_str) == Some(boot.worker_card_id.as_str()))
        .expect("worker card present");
    assert_eq!(worker.get("role").and_then(Value::as_str), Some("worker"));
    assert!(worker.get("runtime").is_some(), "worker card = {worker:?}");
    let worker_runtime: Option<CardRuntimeView> =
        serde_json::from_value(worker["runtime"].clone()).expect("runtime field is typed");
    assert!(worker_runtime.is_none(), "worker card has no runtime row");
}

#[tokio::test]
async fn get_wave_state_callable_by_worker() {
    // Confirms the spec-only soft role gate doesn't fire on read.
    let boot = boot().await;
    let out = call_tool(&boot, TOOL_WAVE_STATE, worker_identity(&boot), json!({}))
        .await
        .expect("worker can also read wave state — no role gate on read");
    assert_eq!(
        out.get("wave")
            .and_then(|w| w.get("id"))
            .and_then(Value::as_str),
        Some(boot.wave_id.as_str()),
    );
}

// ---------------------------------------------------------------------------
// update_wave_state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_wave_state_from_spec_patches_and_emits() {
    let boot = boot().await;

    // Subscribe BEFORE the emit so we don't race the bus.
    let mut rx = boot.ctx.events.subscribe();

    let pre = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();

    let out = call_tool(
        &boot,
        TOOL_UPDATE_WAVE_STATE,
        spec_identity(&boot),
        json!({"title": "patched"}),
    )
    .await
    .expect("spec can update wave state");

    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));
    let wave = out.get("wave").expect("response carries `wave`");
    assert_eq!(
        wave.get("title").and_then(Value::as_str),
        Some("patched"),
        "response reflects the patched title",
    );

    let persisted = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.title, "patched", "row persisted with new title");
    assert!(
        persisted.updated_at >= pre.updated_at,
        "updated_at advanced (or stayed the same on a same-ms boot)",
    );

    // The bus must have seen one wave.updated envelope under the
    // wave's scope.
    let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers within 1s")
        .expect("bus channel not closed");
    assert!(
        matches!(envelope.event, Event::WaveUpdated(ref w) if w.id == boot.wave_id),
        "bus envelope is WaveUpdated for the bound wave: {:?}",
        envelope.event,
    );
    assert!(
        matches!(envelope.scope, EventScope::Wave { ref wave, ref cove }
                 if wave == &boot.wave_id && cove == &boot.cove_id),
        "envelope scope is the wave's scope: {:?}",
        envelope.scope,
    );
}

#[tokio::test]
async fn update_wave_state_from_worker_refused_with_soft_gate() {
    let boot = boot().await;

    // Pre-state for the no-op check.
    let pre = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();

    let err = call_tool(
        &boot,
        TOOL_UPDATE_WAVE_STATE,
        worker_identity(&boot),
        json!({"title": "should-not-stick"}),
    )
    .await
    .expect_err("worker must be denied at MCP entry");

    // -32602 is the soft gate's chosen code (INVALID_PARAMS).
    assert_eq!(err.code, -32602, "soft role gate returns invalid-params");
    assert!(
        err.message.contains("Spec"),
        "error mentions the required role: {err:?}"
    );

    // And nothing was persisted (the DB write never ran).
    let post = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(post.title, pre.title, "wave row unchanged");
    assert_eq!(post.updated_at, pre.updated_at, "updated_at unchanged");
}

#[tokio::test]
async fn update_wave_state_archive_and_unarchive() {
    let boot = boot().await;

    // Archive.
    let out = call_tool(
        &boot,
        TOOL_UPDATE_WAVE_STATE,
        spec_identity(&boot),
        json!({"archived_at": 99999}),
    )
    .await
    .expect("archive ok");
    assert_eq!(
        out.get("wave")
            .and_then(|w| w.get("archived_at"))
            .and_then(Value::as_i64),
        Some(99999),
    );

    // Unarchive.
    let out = call_tool(
        &boot,
        TOOL_UPDATE_WAVE_STATE,
        spec_identity(&boot),
        json!({"archived_at": null}),
    )
    .await
    .expect("unarchive ok");
    assert!(
        out.get("wave")
            .and_then(|w| w.get("archived_at"))
            .map(Value::is_null)
            .unwrap_or(false),
        "archived_at cleared: {out}",
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
        spec_identity(&boot),
        json!({
            "idempotency_key": "job-xyz",
            "status": "accepted",
            "reason": "looks great",
            "message": "accept worker result"
        }),
    )
    .await
    .expect("spec accept verdict ok");
    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));

    let auto = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers auto promotion")
        .expect("bus open");
    assert!(matches!(auto.actor, ActorId::Kernel));
    assert!(matches!(auto.event, Event::WaveUpdated(_)));
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
        "spec's rationale is folded into `result`",
    );
}

#[tokio::test]
async fn legacy_alias_update_task_meta_still_dispatches_via_warn() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    let mut rx = boot.ctx.events.subscribe();

    let out = call_tool(
        &boot,
        "calm.update_task_meta",
        spec_identity(&boot),
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
        spec_identity(&boot),
        json!({
            "idempotency_key": "job-xyz",
            "status": "rejected",
            "reason": "missed acceptance criterion #3",
            "message": "reject worker result"
        }),
    )
    .await
    .expect("spec reject verdict ok");
    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));

    let auto = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers auto promotion")
        .expect("bus open");
    assert!(matches!(auto.actor, ActorId::Kernel));
    assert!(matches!(auto.event, Event::WaveUpdated(_)));
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
        spec_identity(&boot),
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
    .expect_err("worker can't record a spec verdict");
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("Spec"));
}

#[tokio::test]
async fn task_verdict_requires_non_empty_message() {
    let boot = boot().await;

    let err = call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        spec_identity(&boot),
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
        spec_identity(&boot),
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
async fn task_verdict_without_lifecycle_keeps_wave_state_and_records_message() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    let mut rx = boot.ctx.events.subscribe();

    call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        spec_identity(&boot),
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
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Planning);
    let no_more = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(no_more.is_err(), "unexpected lifecycle event: {no_more:?}");
}

#[tokio::test]
async fn task_verdict_lifecycle_legal_emits_wave_updated_and_verdict() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    let mut rx = boot.ctx.events.subscribe();

    call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        spec_identity(&boot),
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

    let lifecycle_env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers lifecycle")
        .expect("bus open");
    match lifecycle_env.event {
        Event::WaveUpdated(payload) => {
            assert_eq!(payload.id, boot.wave_id);
            assert_eq!(payload.lifecycle, WaveLifecycle::Dispatching);
            assert_eq!(
                payload.agent_message.as_deref(),
                Some("accept and dispatch")
            );
        }
        other => panic!("expected WaveUpdated first, got {other:?}"),
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
        other => panic!("expected TaskCompleted second, got {other:?}"),
    }
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Dispatching);
}

#[tokio::test]
async fn task_verdict_lifecycle_illegal_rolls_back_verdict_and_events() {
    let boot = boot().await;
    set_wave_lifecycle(&boot, WaveLifecycle::Planning).await;
    let mut rx = boot.ctx.events.subscribe();

    let err = call_tool(
        &boot,
        TOOL_TASK_VERDICT,
        spec_identity(&boot),
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

    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wave.lifecycle, WaveLifecycle::Planning);
    let no_event = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_event.is_err(),
        "illegal transition emitted event: {no_event:?}"
    );

    let events = boot.repo.events_since(0, Some(100)).await.unwrap();
    assert!(
        events.iter().all(
            |(_, _, _, event)| !matches!(event, Event::TaskCompleted { idempotency_key, .. }
                if idempotency_key == "verdict-illegal-lifecycle")
        ),
        "rolled-back verdict must not be persisted: {events:?}"
    );
}

// ---------------------------------------------------------------------------
// Issue #145 — lifecycle transitions via update_wave_state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn new_wave_defaults_to_draft_lifecycle() {
    let boot = boot().await;
    let wave = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        wave.lifecycle,
        calm_server::model::WaveLifecycle::Draft,
        "freshly minted wave starts in Draft"
    );
}

#[tokio::test]
async fn update_wave_state_lifecycle_happy_path_emits_change_event() {
    let boot = boot().await;
    let mut rx = boot.ctx.events.subscribe();

    let out = call_tool(
        &boot,
        TOOL_UPDATE_WAVE_STATE,
        spec_identity(&boot),
        json!({"lifecycle": "planning"}),
    )
    .await
    .expect("spec can drive draft -> planning");
    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        out.get("wave")
            .and_then(|w| w.get("lifecycle"))
            .and_then(Value::as_str),
        Some("planning"),
        "response carries the new lifecycle"
    );

    // First envelope: #597's kernel auto-promotion. The deprecated
    // handler then emits its historical lifecycle event + WaveUpdated.
    let env1 = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    assert!(matches!(env1.actor, ActorId::Kernel));
    match env1.event {
        Event::WaveUpdated(ref payload) => {
            assert_eq!(payload.id, boot.wave_id);
            assert_eq!(payload.lifecycle, WaveLifecycle::Planning);
            assert_eq!(payload.agent_message, None);
        }
        other => panic!("expected auto WaveUpdated first, got {other:?}"),
    }

    let env2 = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    match env2.event {
        Event::WaveLifecycleChanged {
            ref id,
            ref cove_id,
            from,
            to,
        } => {
            assert_eq!(id, &boot.wave_id);
            assert_eq!(cove_id, &boot.cove_id);
            assert_eq!(from, calm_server::model::WaveLifecycle::Draft);
            assert_eq!(to, calm_server::model::WaveLifecycle::Planning);
        }
        other => panic!("expected WaveLifecycleChanged second, got {other:?}"),
    }

    let env3 = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    assert!(matches!(env3.event, Event::WaveUpdated(_)));

    // DB is also persisted.
    let persisted = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted.lifecycle,
        calm_server::model::WaveLifecycle::Planning
    );
}

#[tokio::test]
async fn update_wave_state_lifecycle_illegal_transition_refused_no_persistence() {
    let boot = boot().await;
    let mut rx = boot.ctx.events.subscribe();

    // draft -> done is illegal regardless of actor — the validator
    // catches it before the row is written.
    let err = call_tool(
        &boot,
        TOOL_UPDATE_WAVE_STATE,
        spec_identity(&boot),
        json!({"lifecycle": "done"}),
    )
    .await
    .expect_err("draft -> done must be refused");
    assert_eq!(err.code, -32403, "forbidden code");
    assert!(
        err.message.to_lowercase().contains("lifecycle"),
        "error mentions lifecycle: {err:?}"
    );

    // DB row unchanged.
    let persisted = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted.lifecycle,
        calm_server::model::WaveLifecycle::Draft,
        "lifecycle still Draft — no row write"
    );

    // No envelope on the bus (commit-then-emit holds when the
    // validator rejects up front).
    let res = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        res.is_err(),
        "no event should be broadcast on illegal transition (got {res:?})"
    );
}

#[tokio::test]
async fn update_wave_state_lifecycle_worker_actor_refused() {
    // Worker cards may not transition lifecycle. The MCP soft gate
    // refuses *any* update_wave_state call from a worker before we
    // even reach validate_transition; this test pins that behavior.
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_UPDATE_WAVE_STATE,
        worker_identity(&boot),
        json!({"lifecycle": "planning"}),
    )
    .await
    .expect_err("worker can't call update_wave_state at all");
    assert_eq!(err.code, -32602);
}

#[tokio::test]
async fn update_wave_state_same_state_lifecycle_is_idempotent_no_event() {
    // Issue #145 followup — a spec resending `lifecycle: <current>`
    // (e.g. on retry after a transient network blip) must succeed
    // silently: no `WaveLifecycleChanged` envelope, no `WaveUpdated`
    // envelope (lifecycle was the only field), and the row's
    // `updated_at` stays put. This pins the idempotent semantics
    // chosen in PR #145 followup.
    let boot = boot().await;
    let mut rx = boot.ctx.events.subscribe();

    let pre = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        pre.lifecycle,
        calm_server::model::WaveLifecycle::Draft,
        "boot fixture lands in Draft",
    );

    let out = call_tool(
        &boot,
        TOOL_UPDATE_WAVE_STATE,
        spec_identity(&boot),
        json!({"lifecycle": "draft"}),
    )
    .await
    .expect("same-state lifecycle is a silent success");
    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        out.get("wave")
            .and_then(|w| w.get("lifecycle"))
            .and_then(Value::as_str),
        Some("draft"),
        "response carries the (unchanged) lifecycle",
    );

    // No bus envelope at all — the row wasn't written and no
    // lifecycle change happened.
    let bus = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        bus.is_err(),
        "no event should be emitted for an idempotent no-op (got {bus:?})",
    );

    // Row untouched: same lifecycle, same `updated_at` (the no-op
    // path returns the existing row without calling `wave_update_tx`).
    let post = boot
        .repo
        .wave_get(boot.wave_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(post.lifecycle, calm_server::model::WaveLifecycle::Draft);
    assert_eq!(
        post.updated_at, pre.updated_at,
        "updated_at must not advance on a lifecycle-only no-op",
    );
}

#[tokio::test]
async fn update_wave_state_same_state_lifecycle_plus_other_field_still_writes() {
    // Companion to the no-op test above: when the spec sends a
    // same-state `lifecycle` together with another genuine change
    // (here: `title`), we still emit `WaveUpdated` for the title
    // change but must NOT emit a `WaveLifecycleChanged` envelope
    // (nothing changed about lifecycle). Pins the boundary case so
    // a future refactor that strips the whole patch on lifecycle
    // no-op surfaces here.
    let boot = boot().await;
    let mut rx = boot.ctx.events.subscribe();

    let out = call_tool(
        &boot,
        TOOL_UPDATE_WAVE_STATE,
        spec_identity(&boot),
        json!({"lifecycle": "draft", "title": "renamed"}),
    )
    .await
    .expect("same-state lifecycle + title change succeeds");
    assert_eq!(
        out.get("wave")
            .and_then(|w| w.get("title"))
            .and_then(Value::as_str),
        Some("renamed"),
    );

    // Two envelopes: kernel auto-promotes Draft -> Planning, then the
    // deprecated handler writes the title. No WaveLifecycleChanged
    // because the requested lifecycle was same-state against the
    // pre-auto row.
    let env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers")
        .expect("bus open");
    assert!(matches!(env.actor, ActorId::Kernel));
    assert!(
        matches!(env.event, Event::WaveUpdated(_)),
        "first envelope is auto WaveUpdated, got {:?}",
        env.event,
    );

    let title_env = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("bus delivers title write")
        .expect("bus open");
    assert!(
        matches!(title_env.event, Event::WaveUpdated(ref w) if w.title == "renamed"),
        "second envelope is title WaveUpdated, got {:?}",
        title_env.event,
    );

    // No follow-up lifecycle-change envelope.
    let bus = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        bus.is_err(),
        "no WaveLifecycleChanged should be emitted for same-state lifecycle (got {bus:?})",
    );
}

#[tokio::test]
async fn update_wave_state_same_state_lifecycle_still_rejects_worker() {
    // Idempotency only applies to actors with lifecycle authority.
    // A Worker resending `lifecycle: <current>` must still be
    // refused — silently accepting would hide a real bug (workers
    // emitting wave-level mutations).
    let boot = boot().await;
    let err = call_tool(
        &boot,
        TOOL_UPDATE_WAVE_STATE,
        worker_identity(&boot),
        json!({"lifecycle": "draft"}),
    )
    .await
    .expect_err("worker still denied even on same-state");
    // Soft role gate at the MCP entry fires first (`-32602`) —
    // the worker can't call `update_wave_state` at all, regardless
    // of payload. The lifecycle validator never runs.
    assert_eq!(err.code, -32602);
}
