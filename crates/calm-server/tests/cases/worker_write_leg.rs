//! #2527, #2528: typed input (`text`, `submit`, `key`, `sequence`) and `message` share one replay
//! contract and one write leg. A replay answers with its first receipt after the binding is
//! proven and before every new-write check; a refusal proven before any byte is not kept, so the
//! same key writes once its cause is gone; with the renderer entry gone there is no receipt to
//! replay. Production MCP socket, renderer, client pump and supervisor writer throughout.
use super::task_terminal::stop;
use super::terminal_support::Harness;
use super::worker_message::{
    Fixture, assert_refusal, expected, message, reads, running, set_status, wait_for_reads, written,
};
use calm_server::db::prelude::*;
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::model::CardRole;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::terminal_interaction::{InputOptions, Target};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The refusal text of a request whose renderer entry is gone.
const NO_RECEIPT: &str = "so this connection holds no receipt to replay: this call wrote nothing, \
     and the outcome of an earlier write under this idempotency_key is unknown to the kernel";

/// Claim and read, so typed input has control and an observation.
async fn claimed_view(h: &Harness, f: &Fixture) -> Value {
    h.ok(
        "neige_terminal_control",
        json!({"attempt_id":f.worker.task,"action":"claim"}),
    )
    .await;
    h.ok(
        "neige_terminal_read",
        json!({"attempt_id":f.worker.task,"wait_ms":50}),
    )
    .await
}

fn typed_args(f: &Fixture, view: &Value, key: &str, text: &str) -> Value {
    json!({"attempt_id":f.worker.task,"observation_id":view["observation_id"],
        "idempotency_key":key,"action":{"type":"text","text":text}})
}

async fn typed(h: &Harness, f: &Fixture, view: &Value, key: &str, text: &str) -> Value {
    h.call("neige_terminal_input", typed_args(f, view, key, text))
        .await
}

#[tokio::test]
async fn input_replay_after_task_parks_returns_receipt() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let view = claimed_view(&h, &f).await;
    let first = typed(&h, &f, &view, "once", "keys").await;
    let receipt = written(&first).clone();
    assert_eq!(wait_for_reads(&f.log, 1).await, vec![b"keys".to_vec()]);
    set_status(&h, &f.worker.task, "done").await;
    let replay = typed(&h, &f, &view, "once", "keys").await;
    assert_eq!(
        written(&replay)["observation_id_used"],
        receipt["observation_id_used"],
        "the first receipt, not a worker_parked refusal: {replay}"
    );
    // A new key on the parked attempt is refused by the write rule.
    let fresh = typed(&h, &f, &view, "new", "more").await;
    assert_refusal(
        &fresh,
        -32403,
        Some("worker_parked"),
        "its worker takes no input",
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(reads(&f.log), vec![b"keys".to_vec()], "0 extra bytes");
    stop(&h, &f.worker).await;
}

/// Run `call` while the renderer's write admission is held, finish the task while its write waits
/// in the writer's queue, then resume admission: the writer refuses it before any byte.
async fn refused_while_queued(
    h: &Harness,
    f: &Fixture,
    hold: impl std::future::Future<Output = tokio::sync::OwnedMutexGuard<()>>,
    call: impl std::future::Future<Output = Value>,
) -> Value {
    let task = f.worker.task.clone();
    let driver = async {
        let guard = hold.await;
        // Let the pump hand the write to the writer, which now waits for admission.
        tokio::time::sleep(Duration::from_millis(300)).await;
        set_status(h, &task, "done").await;
        drop(guard);
    };
    let (reply, ()) = tokio::join!(call, driver);
    assert!(reply.get("error").is_none(), "{reply}");
    let receipt = &reply["result"]["structuredContent"];
    assert_eq!(receipt["outcome"], "refused", "{receipt}");
    assert_eq!(
        receipt["reason"],
        "terminal input control or scope was revoked before write"
    );
    assert!(reads(&f.log).is_empty(), "{:?}", reads(&f.log));
    set_status(h, &task, "running").await;
    reply
}

/// A proven refusal is not kept, for either kind: once the task runs again the same key writes.
#[tokio::test]
async fn refused_write_is_not_kept_and_the_same_key_writes_later() {
    let h = Harness::start().await;
    // Typed input: the caller's connection writes under the control it claimed.
    let f = running(&h, "claude", true, false).await;
    let view = claimed_view(&h, &f).await;
    let entry = h.state.terminal_renderer.get(&f.worker.terminal).unwrap();
    let service = h.interaction();
    let terminal = f.worker.terminal.clone();
    // Held before the call: its write waits for admission once reserved.
    let guard = entry.handle.input_barrier.hold_for_test().await;
    let hold = async {
        let start = Instant::now();
        while !service.input_pending(&terminal).await {
            assert!(start.elapsed() < Duration::from_secs(10), "never reserved");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        guard
    };
    let call = typed(&h, &f, &view, "typed", "keys");
    refused_while_queued(&h, &f, hold, call).await;
    let resent = typed(&h, &f, &view, "typed", "keys").await;
    assert_eq!(written(&resent)["idempotency_key"], "typed");
    assert_eq!(wait_for_reads(&f.log, 1).await, vec![b"keys".to_vec()]);
    stop(&h, &f.worker).await;

    // `message`: the retained kernel delivery client writes.
    let f = running(&h, "codex", true, false).await;
    let slot = Arc::new(Mutex::new(None));
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let registry = h.state.terminal_renderer.clone();
    let seam_slot = slot.clone();
    h.interaction()
        .set_message_write_seam(Box::new(move |terminal: String| {
            Box::pin(async move {
                let entry = registry.get(&terminal).unwrap();
                *seam_slot.lock().unwrap() = Some(entry.handle.input_barrier.hold_for_test().await);
                let _ = entered.send(());
            })
        }));
    let hold = async {
        entered_rx.await.unwrap();
        slot.lock().unwrap().take().unwrap()
    };
    let target = json!({"attempt_id":f.worker.task});
    refused_while_queued(&h, &f, hold, message(&h, target.clone(), "m", "x")).await;
    written(&message(&h, target, "m", "x").await);
    assert_eq!(
        wait_for_reads(&f.log, 1).await,
        vec![expected(&f.worker.task, "x")]
    );
    stop(&h, &f.worker).await;
}

/// With the renderer entry gone there is no connection and no cache: a replay of a written key,
/// typed or message, is told that its earlier outcome is unknown to the kernel.
#[tokio::test]
async fn replay_without_renderer_entry_has_no_receipt() {
    let h = Harness::start().await;
    let f = running(&h, "claude", true, false).await;
    let view = claimed_view(&h, &f).await;
    written(&typed(&h, &f, &view, "typed", "keys").await);
    wait_for_reads(&f.log, 1).await;
    let target = json!({"attempt_id":f.worker.task});
    written(&message(&h, target.clone(), "m", "x").await);
    wait_for_reads(&f.log, 2).await;
    stop(&h, &f.worker).await;
    // As after a server restart until the terminal is reattached (#2499): the entry is gone while
    // the worker session still runs.
    sqlx::query("UPDATE worker_sessions SET state='running',completed_at_ms=NULL WHERE id=?1")
        .bind(&f.worker.session)
        .execute(h.sql.pool())
        .await
        .unwrap();
    for reply in [
        typed(&h, &f, &view, "typed", "keys").await,
        message(&h, target, "m", "x").await,
    ] {
        assert_refusal(&reply, -32403, Some("terminal_unreadable"), NO_RECEIPT);
    }
}

/// The Planner's identity, for calls that must be cancelled in process.
async fn planner(h: &Harness) -> ToolCallIdentity {
    let card = h
        .sql
        .card_identity_get_by_session(&h.session_id)
        .await
        .unwrap()
        .unwrap();
    ToolCallIdentity {
        card_id: card.card_id.to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        session_id: h.session_id.clone(),
        track_id: Some(h.track.clone()),
        area_id: h.area_id.clone(),
        thread_id: "card-bound".into(),
    }
}

/// Park the next write at the enqueue seam (before its slot in the writer's channel) and cancel
/// its request there.
async fn cancel_before_enqueue(
    h: &Harness,
    call: impl std::future::Future<Output = anyhow::Result<Value>>,
) {
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    h.interaction()
        .set_enqueue_seam(Box::new(move |_terminal: String| {
            Box::pin(async move {
                let _ = entered.send(());
                std::future::pending::<()>().await;
            })
        }));
    tokio::select! {
        result = call => panic!("the parked write must not settle: {result:?}"),
        _ = entered_rx => {}
    }
}

/// A request cancelled before its write is enqueued changed nothing, for either kind: no
/// reservation, no retained delivery, no cached receipt. The same key then writes exactly once,
/// and nothing is fenced.
#[tokio::test]
async fn write_cancelled_before_enqueue_leaves_no_fence() {
    let h = Harness::start().await;
    let identity = planner(&h).await;
    let service = h.interaction();
    // Typed input.
    let f = running(&h, "claude", true, false).await;
    let view = claimed_view(&h, &f).await;
    let observation = view["observation_id"].as_str().unwrap().parse().unwrap();
    let target = Target::Attempt(f.worker.task.clone());
    cancel_before_enqueue(
        &h,
        service.input(
            &identity,
            &target,
            Some(observation),
            "typed",
            json!({"type":"text","text":"keys"}),
            InputOptions::default(),
            None,
        ),
    )
    .await;
    assert!(!service.input_pending(&f.worker.terminal).await);
    written(&typed(&h, &f, &view, "typed", "keys").await);
    assert_eq!(wait_for_reads(&f.log, 1).await, vec![b"keys".to_vec()]);
    stop(&h, &f.worker).await;
    // `message`: the kernel writer attached, then the request was cancelled.
    let f = running(&h, "codex", true, false).await;
    let target = Target::Attempt(f.worker.task.clone());
    cancel_before_enqueue(
        &h,
        service.message(
            &identity,
            &target,
            "m",
            json!({"type":"message","text":"x"}),
            None,
        ),
    )
    .await;
    written(&message(&h, json!({"attempt_id":f.worker.task}), "m", "x").await);
    written(&message(&h, json!({"attempt_id":f.worker.task}), "next", "y").await);
    assert_eq!(
        wait_for_reads(&f.log, 2).await,
        vec![expected(&f.worker.task, "x"), expected(&f.worker.task, "y")]
    );
    stop(&h, &f.worker).await;
}

/// A typed write cancelled once enqueued keeps `unknown` cached and the fence up: the same key
/// replays `unknown`, a new key is fenced, and the write lands exactly once when admission resumes.
#[tokio::test]
async fn typed_write_cancelled_after_enqueue_fences_the_next() {
    let h = Harness::start().await;
    let identity = planner(&h).await;
    let service = h.interaction();
    let f = running(&h, "claude", true, false).await;
    let view = claimed_view(&h, &f).await;
    let observation = view["observation_id"].as_str().unwrap().parse().unwrap();
    let entry = h.state.terminal_renderer.get(&f.worker.terminal).unwrap();
    let held = entry.handle.input_barrier.hold_for_test().await;
    let target = Target::Attempt(f.worker.task.clone());
    let call = service.input(
        &identity,
        &target,
        Some(observation),
        "typed",
        json!({"type":"text","text":"keys"}),
        InputOptions::default(),
        None,
    );
    let enqueued = async {
        let start = Instant::now();
        while !service.input_pending(&f.worker.terminal).await {
            assert!(start.elapsed() < Duration::from_secs(10), "never enqueued");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Let the pump hand the write to the writer, which waits for admission.
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    tokio::select! {
        result = call => panic!("the held write must not settle: {result:?}"),
        () = enqueued => {}
    }
    let replay = typed(&h, &f, &view, "typed", "keys").await;
    assert_eq!(
        replay["result"]["structuredContent"]["outcome"], "unknown",
        "{replay}"
    );
    assert_refusal(
        &typed(&h, &f, &view, "next", "more").await,
        -32403,
        None,
        "prior input outcome unknown",
    );
    drop(held);
    assert_eq!(wait_for_reads(&f.log, 1).await, vec![b"keys".to_vec()]);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(reads(&f.log), vec![b"keys".to_vec()], "landed exactly once");
    stop(&h, &f.worker).await;
}
