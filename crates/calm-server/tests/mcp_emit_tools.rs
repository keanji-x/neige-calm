//! PR7a.1 (#136 followup) — integration tests for the three PR7a emit
//! tools (`calm.task.dispatch`, `calm.task.complete`,
//! `calm.task.fail`) over the real MCP server transport.
//!
//! Each test:
//!   * Boots an `McpServer` against an in-memory `SqlxRepo`.
//!   * Mints either a Spec or Worker card (with its per-card MCP token).
//!   * Connects, `initialize`s with the token, then calls one tool.
//!   * Verifies either the retired dispatch shim payload or an event
//!     broadcast frame with the correct actor + scope.
//!
//! Also covers the identity-binding invariant: a `card_id` field
//! smuggled into the tool's `arguments` is IGNORED — the kernel always
//! routes through the `_meta.threadId` mapping.

#![cfg(unix)]

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

// ---------------------------------------------------------------------------
// Per-tool happy paths.
// ---------------------------------------------------------------------------

fn retired_dispatch_payload() -> serde_json::Value {
    json!({
        "error": "calm.task.dispatch was retired (#644); no task was dispatched",
        "migration": {
            "use": "calm.plan.upsert",
            "shape": "{ tasks: [{ key, kind, goal, depends_on?, priority?, gate? }], message }",
            "notes": "The kernel schedules ready tasks and runs verification gates. Use calm.plan.list to see task status."
        }
    })
}

#[tokio::test]
async fn dispatch_request_returns_retired_refusal_without_emitting() {
    let b = boot_with_role(CardRole::Spec).await;
    let mut rx = b.events.subscribe();
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
    assert_eq!(resp["result"]["isError"], json!(false), "{resp:#?}");
    assert_eq!(
        resp["result"]["structuredContent"],
        retired_dispatch_payload()
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(text).expect("text payload is json"),
        retired_dispatch_payload()
    );
    let no_more = timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_more.is_err(),
        "dispatch shim must emit no event: {no_more:?}"
    );
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn legacy_dispatch_alias_inherits_retired_refusal() {
    let b = boot_with_role(CardRole::Spec).await;
    let mut rx = b.events.subscribe();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            11,
            "calm.dispatch_request",
            &b.thread_id,
            json!({
                "kind": "terminal",
                "idempotency_key": "dr-alias",
                "cmd": "echo old",
                "message": "old alias call"
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");
    assert_eq!(
        resp["result"]["structuredContent"],
        retired_dispatch_payload()
    );
    let no_event = timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(
        no_event.is_err(),
        "dispatch alias shim emitted event: {no_event:?}"
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

    let auto_changed = recv_bus(&mut rx).await;
    assert!(matches!(auto_changed.actor, ActorId::Kernel));
    match &auto_changed.event {
        Event::WaveLifecycleChanged {
            id,
            cove_id,
            from,
            to,
            agent_message,
        } => {
            assert_eq!(id, &wave.id);
            assert_eq!(cove_id, &wave.cove_id);
            assert_eq!(*from, WaveLifecycle::Working);
            assert_eq!(*to, WaveLifecycle::Reviewing);
            assert_eq!(agent_message.as_deref(), Some("[auto] first task report"));
        }
        other => panic!("expected auto WaveLifecycleChanged after task report, got {other:?}"),
    }

    let auto_updated = recv_bus(&mut rx).await;
    assert!(matches!(auto_updated.actor, ActorId::Kernel));
    match &auto_updated.event {
        Event::WaveUpdated(payload) => {
            assert_eq!(payload.id, wave.id);
            assert_eq!(payload.lifecycle, WaveLifecycle::Reviewing);
            assert_eq!(
                payload.agent_message.as_deref(),
                Some("[auto] first task report")
            );
        }
        other => panic!("expected auto WaveUpdated after lifecycle change, got {other:?}"),
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
