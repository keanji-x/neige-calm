//! PR7a.1 (#136 followup) — integration tests for the three PR7a emit
//! tools (`calm.task.dispatch`, `calm.task.complete`,
//! `calm.task.fail`) over the real MCP server transport.
//!
//! Each test:
//!   * Boots an `McpServer` against an in-memory `SqlxRepo`.
//!   * Mints either a Spec or Worker card (with its per-card MCP token).
//!   * Connects, `initialize`s with the token, then calls one tool.
//!   * Verifies an event broadcast frame with the correct actor + scope.
//!
//! Also covers the identity-binding invariant: a `card_id` field
//! smuggled into the tool's `arguments` is IGNORED — the kernel always
//! routes through the `_meta.threadId` mapping.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::event::{Event, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::{CardRole, Wave, WaveLifecycle, WavePatch};
use serde_json::json;
use tokio::time::timeout;

mod support;

use support::mcp::{
    CardBoot, TEST_BUDGET, boot_with_role, connect, handshake, recv_frame, send_frame,
    tools_call_frame, wait_for_kind,
};

async fn boot_wave(b: &CardBoot) -> Wave {
    let card = b
        .repo
        .card_get(b.card_id.as_str())
        .await
        .expect("card lookup")
        .expect("boot card exists");
    b.repo
        .wave_get(card.wave_id.as_str())
        .await
        .expect("wave lookup")
        .expect("boot wave exists")
}

async fn set_boot_wave_lifecycle(b: &CardBoot, lifecycle: WaveLifecycle) -> Wave {
    let wave = boot_wave(b).await;
    b.repo
        .wave_update(
            wave.id.as_str(),
            WavePatch {
                lifecycle: Some(lifecycle),
                ..Default::default()
            },
        )
        .await
        .expect("set test wave lifecycle")
}

async fn recv_bus(
    rx: &mut tokio::sync::broadcast::Receiver<calm_server::event::BroadcastEnvelope>,
) -> calm_server::event::BroadcastEnvelope {
    timeout(TEST_BUDGET, rx.recv())
        .await
        .expect("bus envelope within budget")
        .expect("bus open")
}

fn rpc_error_code(resp: &serde_json::Value) -> i64 {
    resp.get("error")
        .and_then(|e| e.get("code"))
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| panic!("response has no error code: {resp:#?}"))
}

// ---------------------------------------------------------------------------
// Per-tool happy paths.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_request_codex_emits_codex_worker_requested() {
    let b = boot_with_role(CardRole::Spec).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            10,
            "calm.task.dispatch",
            &b.thread_id,
            json!({
                "kind": "codex",
                "idempotency_key": "dr-codex-1",
                "goal": "build a thing",
                "message": "dispatch codex worker"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "codex.worker_requested").await;
    // Spec actor → AiSpec(card_id).
    match &env.actor {
        ActorId::AiSpec(cid) => assert_eq!(cid.as_str(), b.card_id.as_str()),
        other => panic!("expected AiSpec actor; got {other:?}"),
    }
    // Scope is Card on the thread-mapped card.
    match &env.scope {
        EventScope::Card { card, .. } => assert_eq!(card.as_str(), b.card_id.as_str()),
        other => panic!("expected Card scope; got {other:?}"),
    }
    // Event carries the goal we sent.
    match &env.event {
        Event::CodexWorkerRequested {
            goal,
            idempotency_key,
            agent_message,
            ..
        } => {
            assert_eq!(goal, "build a thing");
            assert_eq!(idempotency_key, "dr-codex-1");
            assert_eq!(agent_message.as_deref(), Some("dispatch codex worker"));
        }
        other => panic!("expected CodexWorkerRequested; got {other:?}"),
    }
    let _ = &b.server;
}

#[tokio::test]
async fn dispatch_request_requires_non_empty_message() {
    let b = boot_with_role(CardRole::Spec).await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            11,
            "calm.task.dispatch",
            &b.thread_id,
            json!({
                "kind": "codex",
                "idempotency_key": "dr-missing-message",
                "goal": "missing message"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(rpc_error_code(&resp), -32602);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("message must be non-empty"),
        "missing-message error mentions message: {resp:#?}"
    );

    send_frame(
        &mut wr,
        tools_call_frame(
            12,
            "calm.task.dispatch",
            &b.thread_id,
            json!({
                "kind": "codex",
                "idempotency_key": "dr-empty-message",
                "goal": "empty message",
                "message": " \t\n "
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(rpc_error_code(&resp), -32602);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("message must be non-empty"),
        "empty-message error mentions message: {resp:#?}"
    );
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn dispatch_request_without_lifecycle_keeps_non_draft_wave_and_records_message() {
    let b = boot_with_role(CardRole::Spec).await;
    let wave = set_boot_wave_lifecycle(&b, WaveLifecycle::Planning).await;
    let mut rx = b.events.subscribe();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            13,
            "calm.task.dispatch",
            &b.thread_id,
            json!({
                "kind": "codex",
                "idempotency_key": "dr-no-lifecycle",
                "goal": "stay planning",
                "message": "dispatch without lifecycle"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = recv_bus(&mut rx).await;
    match &env.event {
        Event::CodexWorkerRequested {
            idempotency_key,
            agent_message,
            ..
        } => {
            assert_eq!(idempotency_key, "dr-no-lifecycle");
            assert_eq!(agent_message.as_deref(), Some("dispatch without lifecycle"));
        }
        other => panic!("expected CodexWorkerRequested only, got {other:?}"),
    }
    let post = b.repo.wave_get(wave.id.as_str()).await.unwrap().unwrap();
    assert_eq!(post.lifecycle, WaveLifecycle::Planning);
    let no_more = timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(no_more.is_err(), "unexpected extra event: {no_more:?}");
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn dispatch_request_lifecycle_legal_emits_wave_updated_and_request() {
    let b = boot_with_role(CardRole::Spec).await;
    let wave = set_boot_wave_lifecycle(&b, WaveLifecycle::Planning).await;
    let mut rx = b.events.subscribe();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            14,
            "calm.task.dispatch",
            &b.thread_id,
            json!({
                "kind": "codex",
                "idempotency_key": "dr-legal-lifecycle",
                "goal": "dispatch workers",
                "message": "move to dispatching",
                "lifecycle": "dispatching"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let lifecycle_env = recv_bus(&mut rx).await;
    match &lifecycle_env.actor {
        ActorId::AiSpec(card_id) => assert_eq!(card_id.as_str(), b.card_id.as_str()),
        other => panic!("expected AiSpec lifecycle actor; got {other:?}"),
    }
    match &lifecycle_env.event {
        Event::WaveUpdated(payload) => {
            assert_eq!(payload.id, wave.id);
            assert_eq!(payload.lifecycle, WaveLifecycle::Dispatching);
            assert_eq!(
                payload.agent_message.as_deref(),
                Some("move to dispatching")
            );
        }
        other => panic!("expected WaveUpdated first, got {other:?}"),
    }

    let request_env = recv_bus(&mut rx).await;
    match &request_env.event {
        Event::CodexWorkerRequested {
            idempotency_key,
            agent_message,
            ..
        } => {
            assert_eq!(idempotency_key, "dr-legal-lifecycle");
            assert_eq!(agent_message.as_deref(), Some("move to dispatching"));
        }
        other => panic!("expected CodexWorkerRequested second, got {other:?}"),
    }

    let post = b.repo.wave_get(wave.id.as_str()).await.unwrap().unwrap();
    assert_eq!(post.lifecycle, WaveLifecycle::Dispatching);
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn dispatch_request_lifecycle_illegal_rolls_back_batch() {
    let b = boot_with_role(CardRole::Spec).await;
    let wave = set_boot_wave_lifecycle(&b, WaveLifecycle::Planning).await;
    let mut rx = b.events.subscribe();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            15,
            "calm.task.dispatch",
            &b.thread_id,
            json!({
                "kind": "codex",
                "idempotency_key": "dr-illegal-lifecycle",
                "goal": "skip to done",
                "message": "illegal lifecycle",
                "lifecycle": "done"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert_eq!(rpc_error_code(&resp), -32403, "response = {resp:#?}");

    let post = b.repo.wave_get(wave.id.as_str()).await.unwrap().unwrap();
    assert_eq!(post.lifecycle, WaveLifecycle::Planning);
    let no_event = timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_event.is_err(),
        "illegal transition emitted event: {no_event:?}"
    );
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn first_spec_write_auto_promotes_draft_and_second_write_is_idempotent() {
    let b = boot_with_role(CardRole::Spec).await;
    let wave = boot_wave(&b).await;
    assert_eq!(wave.lifecycle, WaveLifecycle::Draft);
    let mut rx = b.events.subscribe();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            16,
            "calm.task.dispatch",
            &b.thread_id,
            json!({
                "kind": "codex",
                "idempotency_key": "dr-auto-1",
                "goal": "first write",
                "message": "first spec write"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let auto = recv_bus(&mut rx).await;
    assert!(matches!(auto.actor, ActorId::Kernel));
    match &auto.event {
        Event::WaveUpdated(payload) => {
            assert_eq!(payload.id, wave.id);
            assert_eq!(payload.lifecycle, WaveLifecycle::Planning);
            assert_eq!(payload.agent_message, None);
        }
        other => panic!("expected auto WaveUpdated first, got {other:?}"),
    }
    assert!(matches!(
        recv_bus(&mut rx).await.event,
        Event::CodexWorkerRequested { .. }
    ));

    send_frame(
        &mut wr,
        tools_call_frame(
            17,
            "calm.task.dispatch",
            &b.thread_id,
            json!({
                "kind": "codex",
                "idempotency_key": "dr-auto-2",
                "goal": "second write",
                "message": "second spec write"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let second = recv_bus(&mut rx).await;
    match &second.event {
        Event::CodexWorkerRequested {
            idempotency_key, ..
        } => assert_eq!(idempotency_key, "dr-auto-2"),
        other => panic!("second write should not re-emit promotion, got {other:?}"),
    }
    let no_more = timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_more.is_err(),
        "unexpected extra promotion event: {no_more:?}"
    );
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn dispatch_request_rejects_worker_identity() {
    let b = boot_with_role(CardRole::Worker).await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            50,
            "calm.task.dispatch",
            &b.thread_id,
            json!({
                "kind": "codex",
                "idempotency_key": "dr-w-1",
                "goal": "x",
                "message": "worker dispatch attempt"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    let err = resp
        .get("error")
        .expect("worker dispatch_request must be rejected");
    let code = err
        .get("code")
        .and_then(|v| v.as_i64())
        .expect("error has code");
    // require_role surfaces as InvalidParams (-32602) — matches the soft
    // role gate convention used by other spec-only MCP tools.
    assert_eq!(code, -32602, "expected spec-only soft gate; got {err:#?}");
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn task_completed_emits_task_completed_with_worker_actor() {
    let b = boot_with_role(CardRole::Worker).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            20,
            "calm.task.complete",
            &b.thread_id,
            json!({"idempotency_key": "tc-1", "result": {"ok": true}}),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "task.completed").await;
    match &env.actor {
        ActorId::AiCodex(cid) => assert_eq!(cid.as_str(), b.card_id.as_str()),
        other => panic!("expected AiCodex actor; got {other:?}"),
    }
    match &env.scope {
        EventScope::Card { card, .. } => assert_eq!(card.as_str(), b.card_id.as_str()),
        other => panic!("expected Card scope; got {other:?}"),
    }
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn legacy_alias_task_completed_still_dispatches_via_warn() {
    let b = boot_with_role(CardRole::Worker).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            21,
            "calm.task_completed",
            &b.thread_id,
            json!({"idempotency_key": "tc-legacy", "result": {"ok": true}}),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "task.completed").await;
    match &env.actor {
        ActorId::AiCodex(cid) => assert_eq!(cid.as_str(), b.card_id.as_str()),
        other => panic!("expected AiCodex actor; got {other:?}"),
    }
    match &env.event {
        Event::TaskCompleted {
            idempotency_key, ..
        } => assert_eq!(idempotency_key, "tc-legacy"),
        other => panic!("expected TaskCompleted; got {other:?}"),
    }
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn task_completed_from_working_auto_promotes_wave_to_reviewing() {
    let b = boot_with_role(CardRole::Worker).await;
    let wave = set_boot_wave_lifecycle(&b, WaveLifecycle::Working).await;
    let mut rx = b.events.subscribe();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            22,
            "calm.task.complete",
            &b.thread_id,
            json!({"idempotency_key": "tc-auto-review", "result": {"ok": true}}),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let task_env = recv_bus(&mut rx).await;
    match &task_env.actor {
        ActorId::AiCodex(card_id) => assert_eq!(card_id.as_str(), b.card_id.as_str()),
        other => panic!("expected worker actor first; got {other:?}"),
    }
    assert!(matches!(
        task_env.event,
        Event::TaskCompleted {
            ref idempotency_key,
            ..
        } if idempotency_key == "tc-auto-review"
    ));

    let auto = recv_bus(&mut rx).await;
    assert!(matches!(auto.actor, ActorId::Kernel));
    match &auto.event {
        Event::WaveUpdated(payload) => {
            assert_eq!(payload.id, wave.id);
            assert_eq!(payload.lifecycle, WaveLifecycle::Reviewing);
            assert_eq!(
                payload.agent_message.as_deref(),
                Some("[auto] first task report")
            );
        }
        other => panic!("expected auto WaveUpdated after task report, got {other:?}"),
    }
    let post = b.repo.wave_get(wave.id.as_str()).await.unwrap().unwrap();
    assert_eq!(post.lifecycle, WaveLifecycle::Reviewing);
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn task_failed_emits_task_failed_with_worker_actor() {
    let b = boot_with_role(CardRole::Worker).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            30,
            "calm.task.fail",
            &b.thread_id,
            json!({"idempotency_key": "tf-1", "reason": "stub failure"}),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "task.failed").await;
    match &env.actor {
        ActorId::AiCodex(cid) => assert_eq!(cid.as_str(), b.card_id.as_str()),
        other => panic!("expected AiCodex actor; got {other:?}"),
    }
    match &env.event {
        Event::TaskFailed { reason, .. } => assert_eq!(reason, "stub failure"),
        other => panic!("expected TaskFailed; got {other:?}"),
    }
    let _ = (&b.server, &b.repo);
}

// ---------------------------------------------------------------------------
// Identity binding: smuggled `card_id` arg is ignored.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn smuggled_card_id_in_args_is_ignored() {
    // The transport binds the identity at handshake — sending a
    // `card_id` field in `arguments` must not let the caller claim a
    // different card. Defense-in-depth assertion against a future
    // refactor that accidentally trusts tool args for identity.
    let b = boot_with_role(CardRole::Worker).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            40,
            "calm.task.complete",
            &b.thread_id,
            json!({
                "idempotency_key": "tc-smuggle",
                "card_id": b.other_card_id, // <-- smuggled
                "actor": "ai_spec",          // <-- smuggled
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "task.completed").await;
    // The actor / scope must still bind to the connection's card
    // (b.card_id), NOT the smuggled other_card_id.
    match &env.actor {
        ActorId::AiCodex(cid) => assert_eq!(
            cid.as_str(),
            b.card_id.as_str(),
            "smuggled card_id must not override identity binding"
        ),
        other => panic!("expected AiCodex actor; got {other:?}"),
    }
    match &env.scope {
        EventScope::Card { card, .. } => assert_eq!(
            card.as_str(),
            b.card_id.as_str(),
            "smuggled card_id must not change the event scope"
        ),
        other => panic!("expected Card scope; got {other:?}"),
    }
    let _ = (&b.server, &b.repo);
}

// ---------------------------------------------------------------------------
// End-to-end: dispatch_request → dispatcher → spawn failure → rollback.
// ---------------------------------------------------------------------------
//
// History: this test originally asserted the buggy pre-#310 behavior —
// that the dispatcher's `card_with_codex_create_tx` committed a worker
// card row BEFORE the terminal spawn was attempted, so even with a bogus
// proc-supervisor socket the orphan card would land in `cards_by_wave`
// and be observable. PR #312 (which fixes #310) flips that ordering:
// the two-stage spawn defers `CardAdded` emission until after the
// renderer entry is registered, AND worker compensation actively DELETEs
// the pre-committed card row when the spawn errors out.
//
// Under the new semantics, driving `calm.task.dispatch[codex]`
// against a missing proc-supervisor socket must:
//   (a) return success from the MCP tool (the spec just enqueued a
//       `codex.worker_requested` event — the failure is downstream),
//   (b) emit a `TaskFailed` event with the dispatch's `idempotency_key`
//       on the bus once the dispatcher drains the request and the spawn
//       fails, and
//   (c) leave NO worker card with that idempotency_key in the cards
//       table — proof the rollback fired.
//
// This is the MCP-driven mirror of
// `dispatcher_rolls_back_card_on_codex_daemon_spawn_failure_issue_310`
// in `tests/dispatcher.rs`, which already pins the dispatcher-level
// contract. This one asserts the same contract end-to-end through the
// real MCP transport.

#[tokio::test]
async fn dispatch_request_drives_dispatcher_rollback_on_stub_daemon() {
    let b = boot_with_role(CardRole::Spec).await;

    // Stand up the dispatcher on the same bus + repo with a missing
    // proc-supervisor socket so every EnsureProc attempt fails.
    let cache = CardRoleCache::new();
    b.repo.seed_card_role_cache(&cache).await.unwrap();
    let wcc = calm_server::wave_cove_cache::WaveCoveCache::new();
    b.repo.seed_wave_cove_cache(&wcc).await.unwrap();
    // #272 (N3) — Dispatcher now stores `Weak<CodexClient>` so the
    // caller MUST hold the strong Arc for the dispatcher's lifetime.
    // The local `codex` binding's natural drop at end-of-test releases
    // the strong reference.
    let codex = Arc::new(calm_server::state::CodexClient::new_stub());
    let _dispatcher = calm_server::dispatcher::Dispatcher::spawn(
        b.repo.clone(),
        b.events.clone(),
        calm_server::state::WriteContext::new(cache.clone(), wcc),
        codex.clone(),
        Arc::new(calm_server::state::DaemonClient {
            data_dir: PathBuf::from("/tmp/neige-mcp-e2e-noop"),
            proc_supervisor_sock: Some(PathBuf::from(
                "/tmp/neige-mcp-e2e-missing-proc-supervisor.sock",
            )),
        }),
        None,
        calm_server::shared_codex_appserver::SharedCodexAppServer::new_stub(b.repo.clone()),
        2,
    );

    // Subscribe BEFORE dispatch so we don't miss the TaskFailed emit.
    let mut rx = b.events.subscribe();

    let idem = "e2e-rollback-1";
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;
    send_frame(
        &mut wr,
        tools_call_frame(
            50,
            "calm.task.dispatch",
            &b.thread_id,
            json!({
                "kind": "codex",
                "idempotency_key": idem,
                "goal": "e2e worker spawn",
                "message": "enqueue e2e worker"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    // (a) MCP tool itself succeeds — it merely enqueued the request.
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    // (b) Wait for the dispatcher to drain, fail the spawn, run worker
    // compensation, and emit `TaskFailed` for our key.
    let deadline = tokio::time::Instant::now() + TEST_BUDGET;
    let mut saw_failed = false;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match timeout(remaining, rx.recv()).await {
            Ok(Ok(env)) => {
                if let Event::TaskFailed {
                    idempotency_key, ..
                } = &env.event
                    && idempotency_key == idem
                {
                    saw_failed = true;
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    assert!(
        saw_failed,
        "expected dispatcher to emit task.failed for {idem} after stub-daemon spawn failure"
    );

    // (c) No worker card with our idempotency_key remains — the
    // rollback deleted the pre-committed row.
    let spec = b.repo.card_get(b.card_id.as_str()).await.unwrap().unwrap();
    let wave_id_str = spec.wave_id.as_str().to_string();
    let cards = b.repo.cards_by_wave(&wave_id_str).await.unwrap();
    let leftover: Vec<_> = cards
        .iter()
        .filter(|c| c.payload.get("idempotency_key").and_then(|v| v.as_str()) == Some(idem))
        .collect();
    assert!(
        leftover.is_empty(),
        "expected worker card to be rolled back after spawn failure; \
         found {} leftover card(s): {:?}",
        leftover.len(),
        leftover.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
    );
    let _ = &b.server;
}
