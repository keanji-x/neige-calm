//! #2348 A3: a Claude Planner's tool approvals. A real `ClaudePlannerSession` drives the fake
//! `claude` (scenarios `ask`, `ask-cancel`, `ask-exit`); the session tests read what it pushes to
//! the held-request channel the way a harness does, and the stack tests run the production server:
//! the permission-mode route, a hold ask, and the answer route back to the CLI's stdin.

use std::time::Duration;

use axum::http::StatusCode;
use calm_server::event::{AskDelivery, AskQuestion, Event};
use serde_json::{Value, json};

use super::claude_planner_stack_fixture::{Root, Stack};

const ALLOW_LINE: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"perm-1","response":{"behavior":"allow"}}}"#;
fn question() -> AskQuestion {
    AskQuestion {
        title: "Bash: echo z > /probe/outside/denied.txt\nBlocked path: /probe/outside/denied.txt"
            .into(),
        options: vec!["Allow".into(), "Deny".into()],
    }
}

/// A stored mode that cannot be read refuses the turn before anything is spawned, and tells the
/// reader so; it is never taken for either mode. The harness reads it once for every provider
/// when it issues the turn (#2348).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreadable_permission_mode_refuses_the_turn() {
    let root = Root::new("ask");
    let stack = Stack::boot(&root).await;
    let (_track, card) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card).await;
    let pool = stack.repo().sqlite_db().expect("sqlite");
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.permission_mode', 'yolo') WHERE id = ?1",
    )
    .bind(&card)
    .execute(pool.writer_pool())
    .await
    .expect("corrupt the mode");
    let spawned = root.read_fake("spawns");
    let harness = stack.harness(&runtime.id);
    let (status, body) = stack.post_input(&card, "hello").await;
    assert_eq!(status, StatusCode::OK, "the message is queued: {body}");
    let mut block = None;
    for _ in 0..200 {
        block = harness.issuance_block().await;
        if block.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let block = block.expect("the reader is told why nothing is sent");
    assert!(block.contains("permission mode"), "{block}");
    assert_eq!(root.read_fake("spawns"), spawned, "nothing was spawned");
    stack.shutdown().await;
}

async fn ask_rows(stack: &Stack, track: &str, kind: &str) -> Vec<(i64, Event)> {
    stack
        .repo()
        .events_for_track(track, &[kind], None)
        .await
        .expect("events")
        .into_iter()
        .map(|row| (row.id, row.event))
        .collect()
}

async fn wait_rows(stack: &Stack, track: &str, kind: &str, n: usize) -> Vec<(i64, Event)> {
    for _ in 0..400 {
        let rows = ask_rows(stack, track, kind).await;
        if rows.len() >= n {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{kind} never reached {n} rows");
}

/// A Claude Planner track in `ask` mode whose next turn runs `scenario`.
async fn ask_stack(scenario: &str) -> (Root, Stack, String, String) {
    let root = Root::new(scenario);
    let stack = Stack::boot(&root).await;
    let (track, card) = stack.create_claude_track().await;
    let (status, body) = stack
        .send(
            "PUT",
            &format!("/api/cards/{card}/planner/permission-mode"),
            Some(json!({"permission_mode": "ask"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = stack.post_input(&card, "approve me").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (root, stack, track, card)
}

/// The production path: the route set `ask`, the CLI's `can_use_tool` became a `hold` ask, and the
/// answer route's Allow reached the CLI as exactly `{"behavior":"allow"}`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_approval_is_a_hold_ask_answered_through_the_route() {
    let (root, stack, track, card) = ask_stack("ask").await;
    let asks = wait_rows(&stack, &track, "ask.requested", 1).await;
    let (
        ask_id,
        Event::AskRequested {
            questions,
            delivery,
            ..
        },
    ) = &asks[0]
    else {
        panic!("{asks:?}");
    };
    assert_eq!(*delivery, AskDelivery::Hold);
    assert_eq!(questions, &vec![question()]);
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/tracks/{track}/asks/{ask_id}/answer"),
            Some(json!({"answers": [{"option": 0}]})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let outcomes = stack.wait_outcomes(&card, 1).await;
    assert_eq!(
        outcomes[0]["status"],
        Value::from("completed"),
        "{outcomes:?}"
    );
    let stdin = root.read_fake("stdin").expect("stdin");
    assert_eq!(stdin.lines().last(), Some(ALLOW_LINE));
    assert!(ask_rows(&stack, &track, "ask.withdrawn").await.is_empty());
    stack.shutdown().await;
}

/// A request the CLI cancels is withdrawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_request_withdraws_its_ask() {
    let (_root, stack, track, _card) = ask_stack("ask-cancel").await;
    let asks = wait_rows(&stack, &track, "ask.requested", 1).await;
    let withdrawn = wait_rows(&stack, &track, "ask.withdrawn", 1).await;
    assert!(
        matches!(&withdrawn[0].1, Event::AskWithdrawn { ask_id, .. } if *ask_id == asks[0].0),
        "{withdrawn:?}"
    );
    stack.shutdown().await;
}

/// A request read after the CLI exited still asks, and is withdrawn with the spawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_read_after_the_exit_asks_and_is_withdrawn() {
    let (_root, stack, track, _card) = ask_stack("ask-exit").await;
    let asks = wait_rows(&stack, &track, "ask.requested", 1).await;
    let withdrawn = wait_rows(&stack, &track, "ask.withdrawn", 1).await;
    assert!(
        matches!(&withdrawn[0].1, Event::AskWithdrawn { ask_id, .. } if *ask_id == asks[0].0),
        "{withdrawn:?}"
    );
    stack.shutdown().await;
}
