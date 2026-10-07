//! #2348 A3: a Claude Planner's tool approvals. A real `ClaudePlannerSession` drives the fake
//! `claude` (scenarios `ask`, `ask-cancel`, `ask-exit`); the session tests read what it pushes to
//! the held-request channel the way a harness does, and the stack tests run the production server:
//! the permission-mode route, a hold ask, and the answer route back to the CLI's stdin.

use std::time::Duration;

use axum::http::StatusCode;
use calm_server::event::{AskDelivery, AskQuestion, Event};
use calm_server::harness::held_requests::{ConnectionId, HeldRequestMessage, RequestKey};
use calm_server::planner_model::TurnModelSelection;
use calm_server::planner_permission_mode::PlannerPermissionMode;
use serde_json::{Value, json};

use super::claude_planner_session_fixture::{Rig, client_id, until_completed};
use super::claude_planner_stack_fixture::{Root, Stack};

const ALLOW_LINE: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"perm-1","response":{"behavior":"allow"}}}"#;
const DENY_LINE: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"perm-1","response":{"behavior":"deny","message":"The user denied this tool use."}}}"#;

fn question() -> AskQuestion {
    AskQuestion {
        title: "Bash: echo z > /probe/outside/denied.txt\nBlocked path: /probe/outside/denied.txt"
            .into(),
        options: vec!["Allow".into(), "Deny".into()],
    }
}

/// Start one turn of `rig` under `scenario`; returns its turn id, the spawn's connection.
async fn start(rig: &Rig, scenario: &str) -> String {
    std::fs::write(rig.bin("scenario"), scenario).expect("scenario");
    rig.session()
        .turn_start(
            &rig.thread,
            rig.text("approve me"),
            &TurnModelSelection::inherit(),
            &client_id(),
            None,
        )
        .await
        .expect("turn_start")
}

/// The fake's record of its stdin lines after the user line.
fn answers(rig: &Rig) -> Vec<String> {
    rig.read_bin("stdin")
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.contains(r#""type":"user""#))
        .map(str::to_string)
        .collect()
}

/// The spawn's argv as the fake recorded it.
fn argv(rig: &Rig) -> Vec<String> {
    rig.read_bin("argv")
        .expect("argv")
        .lines()
        .map(str::to_string)
        .collect()
}

fn expect_open(
    message: HeldRequestMessage,
    turn: &str,
) -> Box<dyn calm_server::harness::held_requests::HeldResponder> {
    match message {
        HeldRequestMessage::Open {
            request_key,
            connection,
            questions,
            responder,
        } => {
            assert_eq!(request_key, RequestKey("perm-1".into()));
            assert_eq!(connection, ConnectionId(turn.into()));
            assert_eq!(questions, vec![question()]);
            responder
        }
        _ => panic!("expected Open first"),
    }
}

fn expect_connection_lost(message: HeldRequestMessage, turn: &str) {
    match message {
        HeldRequestMessage::ConnectionLost { connection } => {
            assert_eq!(connection, ConnectionId(turn.into()))
        }
        HeldRequestMessage::Open { .. } => panic!("expected ConnectionLost, got Open"),
        HeldRequestMessage::Gone { .. } => panic!("expected ConnectionLost, got Gone"),
    }
}

/// Each answer reaches the CLI as exactly the decision: no `updatedInput`, no
/// `updatedPermissions`, none of the CLI's `permission_suggestions` echoed back. A responder
/// dropped unanswered denies.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ask_spawn_holds_each_can_use_tool_and_writes_the_answer_on_stdin() {
    let rig = Rig::new("ask").await;
    rig.set_permission_mode(PlannerPermissionMode::Ask).await;
    let mut rx = rig.session().subscribe_events();
    let not_answered = r#"{"type":"control_response","response":{"subtype":"success","request_id":"perm-1","response":{"behavior":"deny","message":"This tool use was not approved: nobody answered the request for it."}}}"#;
    for (answer, line) in [
        (Some(0), ALLOW_LINE),
        (Some(1), DENY_LINE),
        (None, not_answered),
    ] {
        let turn = start(&rig, "ask").await;
        let responder = expect_open(rig.next_held().await, &turn);
        match answer {
            Some(option) => responder.respond(option),
            None => drop(responder),
        }
        until_completed(&mut rx).await;
        expect_connection_lost(rig.next_held().await, &turn);
        assert_eq!(answers(&rig).last().map(String::as_str), Some(line));
        let argv = argv(&rig);
        let prompt = argv
            .iter()
            .position(|arg| arg == "--permission-prompt-tool");
        assert_eq!(prompt.map(|at| argv[at + 1].as_str()), Some("stdio"));
        assert!(!argv.iter().any(|arg| arg == "--permission-prompts"));
    }
    assert!(rig.held_is_empty().await);
}

/// `never` keeps today's spawn and answers a stray `can_use_tool` itself; nothing reaches the
/// held-request channel, not even the end of the spawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_never_spawn_answers_can_use_tool_itself() {
    let rig = Rig::new("ask").await;
    let mut rx = rig.session().subscribe_events();
    start(&rig, "ask").await;
    until_completed(&mut rx).await;
    assert_eq!(
        answers(&rig),
        [
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"perm-1","response":{"behavior":"deny","message":"this Planner has no approval surface"}}}"#
        ]
    );
    let argv = argv(&rig);
    let prompts = argv.iter().position(|arg| arg == "--permission-prompts");
    assert_eq!(prompts.map(|at| argv[at + 1].as_str()), Some("none"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(rig.held_is_empty().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_can_use_tool_is_gone() {
    let rig = Rig::new("ask-cancel").await;
    rig.set_permission_mode(PlannerPermissionMode::Ask).await;
    let mut rx = rig.session().subscribe_events();
    let turn = start(&rig, "ask-cancel").await;
    let responder = expect_open(rig.next_held().await, &turn);
    match rig.next_held().await {
        HeldRequestMessage::Gone { request_key } => {
            assert_eq!(request_key, RequestKey("perm-1".into()))
        }
        _ => panic!("expected Gone"),
    }
    until_completed(&mut rx).await;
    expect_connection_lost(rig.next_held().await, &turn);
    drop(responder);
}

/// The CLI may write a `can_use_tool` and exit before it is read: the read goes on after the exit,
/// the request still opens, and the spawn's connection ends only after it, once the read task is
/// done (which is after `TurnCompleted`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_can_use_tool_read_after_the_exit_opens_before_the_connection_is_lost() {
    let rig = Rig::new("ask-exit").await;
    rig.set_permission_mode(PlannerPermissionMode::Ask).await;
    let mut rx = rig.session().subscribe_events();
    let turn = start(&rig, "ask-exit").await;
    let responder = expect_open(rig.next_held().await, &turn);
    until_completed(&mut rx).await;
    expect_connection_lost(rig.next_held().await, &turn);
    drop(responder);
}

/// A stored mode that cannot be read refuses the turn before anything is spawned, and tells the
/// reader so; it is never taken for either mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreadable_permission_mode_refuses_the_turn() {
    let rig = Rig::new("ask").await;
    sqlx::query(
        "UPDATE cards SET payload = json_set(payload, '$.permission_mode', 'full') WHERE id = ?1",
    )
    .bind(&rig.card_id)
    .execute(rig.repo.pool())
    .await
    .expect("corrupt the mode");
    let refused = rig
        .session()
        .turn_start(
            &rig.thread,
            rig.text("hello"),
            &TurnModelSelection::inherit(),
            &client_id(),
            None,
        )
        .await;
    match refused {
        Err(calm_server::harness::backend::TurnStartFailure::Refused { reader, .. }) => {
            assert!(reader.contains("permission mode"), "{reader}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(rig.read_bin("spawns").is_none(), "nothing was spawned");
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
