//! Integration tests for the emit tools (`neige.task.complete`, `neige.task.fail`) over the real MCP
//! server transport.

#![cfg(unix)]

use calm_server::event::{Event, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::CardRole;
use serde_json::json;

use crate::support;

use support::mcp::{
    CardBoot, boot_with_role, cli_output, connect, handshake, neige_cli_via_socket, recv_frame,
    send_frame, tools_call_frame, wait_for_kind,
};

fn tools_call_frame_no_thread(id: i64, name: &str, args: serde_json::Value) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": args,
        }
    })
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
            "neige.task.complete",
            &b.thread_id,
            json!({"attempt_id": "tc-1", "result": {"ok": true}}),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "task.completed").await;
    match &env.actor {
        ActorId::AiCodexSession(sid) => assert_eq!(sid.as_str(), b.session_id.as_str()),
        other => panic!("expected AiCodexSession actor; got {other:?}"),
    }
    match &env.scope {
        EventScope::Card { card, .. } => assert_eq!(card.as_str(), b.card_id.as_str()),
        other => panic!("expected Card scope; got {other:?}"),
    }
    let _ = (&b.server, &b.repo);
}

#[tokio::test]
async fn consumer_summary_authenticated_completion_preserves_entire_result() {
    let b = boot_with_role(CardRole::Worker).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;
    let result = json!({"$neige_result_presentation":"worker-summary-v1",
        "summary":"worker claims sum=10 and 10 tests OK",
        "details":{"direct_call_command":"script\n".repeat(1024)}});
    send_frame(
        &mut wr,
        tools_call_frame(
            20,
            "neige.task.complete",
            &b.thread_id,
            json!({"attempt_id":"consumer-attempt", "result":result}),
        ),
    )
    .await;
    let response = recv_frame(&mut rd).await;
    assert!(response.get("error").is_none(), "{response}");
    let event = wait_for_kind(&mut rx, "task.completed").await;
    assert_eq!(
        event.actor,
        ActorId::AiCodexSession(b.session_id.clone().into())
    );
    let EventScope::Card { card, .. } = &event.scope else {
        panic!("worker card scope required")
    };
    assert_eq!(card.as_str(), b.card_id);
    let Event::TaskCompleted {
        idempotency_key,
        result: recorded,
        ..
    } = &event.event
    else {
        panic!("completion required")
    };
    assert_eq!(idempotency_key, "consumer-attempt");
    assert_eq!(recorded, &result);
}

#[tokio::test]
async fn task_completed_from_claude_worker_persists_claude_session_actor() {
    let b = boot_with_role(CardRole::Worker).await;
    let pool = b.repo.sqlite_pool().expect("sqlite pool");
    sqlx::query("UPDATE worker_sessions SET provider = 'claude' WHERE id = ?1")
        .bind(&b.session_id)
        .execute(&pool)
        .await
        .expect("flip test session provider to claude");
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame_no_thread(
            23,
            "neige.task.complete",
            json!({"attempt_id": "tc-claude", "result": {"ok": true}}),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "task.completed").await;
    match &env.actor {
        ActorId::AiClaudeSession(sid) => assert_eq!(sid.as_str(), b.session_id.as_str()),
        other => panic!("expected AiClaudeSession actor; got {other:?}"),
    }
    let actor_text: String = sqlx::query_scalar(
        r#"SELECT actor
             FROM events
            WHERE kind = 'task.completed'
              AND json_extract(payload, '$.idempotency_key') = 'tc-claude'
            ORDER BY id DESC
            LIMIT 1"#,
    )
    .fetch_one(&pool)
    .await
    .expect("persisted task.completed actor");
    let actor: ActorId = serde_json::from_str(&actor_text).expect("events.actor is ActorId JSON");
    match actor {
        ActorId::AiClaudeSession(sid) => assert_eq!(sid.as_str(), b.session_id.as_str()),
        other => panic!("persisted actor must be AiClaudeSession; got {other:?}"),
    }
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
            "neige.task.fail",
            &b.thread_id,
            json!({"attempt_id": "tf-1", "reason": "stub failure"}),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "task.failed").await;
    match &env.actor {
        ActorId::AiCodexSession(sid) => assert_eq!(sid.as_str(), b.session_id.as_str()),
        other => panic!("expected AiCodexSession actor; got {other:?}"),
    }
    match &env.event {
        Event::TaskFailed { reason, .. } => assert_eq!(reason, "stub failure"),
        other => panic!("expected TaskFailed; got {other:?}"),
    }
    let _ = (&b.server, &b.repo);
}

async fn task_failed_event_count(b: &CardBoot) -> i64 {
    let pool = b.repo.sqlite_pool().expect("sqlite pool");
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'task.failed'")
        .fetch_one(&pool)
        .await
        .expect("count task.failed events")
}

/// #1801 F5: the tool is the one source for the blank-reason rule; MCP and `neige task-failed` both meet it.
#[tokio::test]
async fn task_fail_rejects_blank_reason() {
    let b = boot_with_role(CardRole::Worker).await;
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;
    for (id, reason) in [(40, ""), (41, "  "), (42, "\t\n")] {
        send_frame(
            &mut wr,
            tools_call_frame_no_thread(
                id,
                "neige.task.fail",
                json!({"attempt_id": "tf-blank", "reason": reason}),
            ),
        )
        .await;
        let resp = recv_frame(&mut rd).await;
        assert_eq!(
            resp["error"]["code"],
            json!(-32602),
            "{reason:?}: {resp:#?}"
        );
        assert_eq!(
            resp["error"]["message"],
            json!("task_fail: missing `reason` (non-empty)"),
            "{reason:?}"
        );
    }

    let resp = neige_cli_via_socket(
        &b.socket_path,
        &b.raw_token,
        &["task-failed", "--attempt-id", "tf-blank", "--reason", "  "],
    )
    .await;
    let (stdout, stderr, exit) = cli_output(&resp);
    assert_eq!(exit, 4, "stderr = {stderr}");
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "neige: neige.task.fail: task_fail: missing `reason` (non-empty) (code -32602)\n"
    );
    assert_eq!(
        task_failed_event_count(&b).await,
        0,
        "a blank reason wrote an event"
    );
}

#[tokio::test]
async fn smuggled_card_id_in_args_is_ignored() {
    // The transport binds the identity at handshake; a `card_id` field in `arguments` must not let
    // the caller claim a different card.
    let b = boot_with_role(CardRole::Worker).await;
    let mut rx = b.events.subscribe_filtered();
    let (mut rd, mut wr) = connect(&b.socket_path).await;
    handshake(&mut rd, &mut wr, &b.raw_token).await;

    send_frame(
        &mut wr,
        tools_call_frame(
            40,
            "neige.task.complete",
            &b.thread_id,
            json!({
                "attempt_id": "tc-smuggle",
                "card_id": b.other_card_id, // <-- smuggled
                "actor": "ai_planner",          // <-- smuggled
            }),
        ),
    )
    .await;
    let resp = recv_frame(&mut rd).await;
    assert!(resp.get("error").is_none(), "tool errored: {resp:#?}");

    let env = wait_for_kind(&mut rx, "task.completed").await;
    match &env.actor {
        ActorId::AiCodexSession(sid) => assert_eq!(
            sid.as_str(),
            b.session_id.as_str(),
            "smuggled card_id must not override session identity binding"
        ),
        other => panic!("expected AiCodexSession actor; got {other:?}"),
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
