#![cfg(test)]
//! #2348 A3: a Claude Planner's tool approvals. A real `ClaudePlannerSession` drives the fake
//! `claude` (scenarios `ask`, `ask-cancel`, `ask-exit`); the session tests read what it pushes to
//! the held-request channel the way a harness does, and the stack tests run the production server:
//! the permission-mode route, a hold ask, and the answer route back to the CLI's stdin.

use super::{ordinary_start, request};

use std::time::Duration;

use calm_server::event::AskQuestion;
use calm_server::harness::held_requests::{ConnectionId, HeldRequestMessage, RequestKey};
use calm_server::planner_model::TurnModelSelection;
use calm_server::planner_permission_mode::PlannerPermissionMode;

use super::claude_planner_session_fixture::{Rig, client_id, until_completed};

const ALLOW_LINE: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"perm-1","response":{"behavior":"allow"}}}"#;
const DENY_LINE: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"perm-1","response":{"behavior":"deny","message":"The user denied this tool use."}}}"#;

fn question() -> AskQuestion {
    AskQuestion {
        title: "Bash: echo z > /probe/outside/denied.txt\nBlocked path: /probe/outside/denied.txt"
            .into(),
        options: vec!["Allow".into(), "Deny".into()],
    }
}

/// Start one turn of `rig` under `scenario` and the permission mode the harness read for it;
/// returns its turn id, the spawn's connection.
async fn start(rig: &Rig, scenario: &str, mode: PlannerPermissionMode) -> String {
    std::fs::write(rig.bin("scenario"), scenario).expect("scenario");
    rig.session()
        .start_turn(
            &rig.thread,
            request(
                rig.text("approve me"),
                &TurnModelSelection::inherit(),
                mode,
                &client_id(),
            ),
        )
        .await
        .and_then(ordinary_start)
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
    let mut rx = rig.session().subscribe_events();
    let not_answered = r#"{"type":"control_response","response":{"subtype":"success","request_id":"perm-1","response":{"behavior":"deny","message":"This tool use was not approved: nobody answered the request for it."}}}"#;
    for (answer, line) in [
        (Some(0), ALLOW_LINE),
        (Some(1), DENY_LINE),
        (None, not_answered),
    ] {
        let turn = start(&rig, "ask", PlannerPermissionMode::Ask).await;
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
/// held-request channel, not even the end of the spawn. `full` (#2441) answers it the same way,
/// from a spawn that bypasses every check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_never_or_full_spawn_answers_can_use_tool_itself() {
    for (mode, permission_mode) in [
        (PlannerPermissionMode::Never, None),
        (PlannerPermissionMode::Full, Some("bypassPermissions")),
    ] {
        let rig = Rig::new("ask").await;
        let mut rx = rig.session().subscribe_events();
        start(&rig, "ask", mode).await;
        until_completed(&mut rx).await;
        assert_eq!(
            answers(&rig),
            [
                r#"{"type":"control_response","response":{"subtype":"success","request_id":"perm-1","response":{"behavior":"deny","message":"this Planner has no approval surface"}}}"#
            ],
            "{mode:?}"
        );
        let argv = argv(&rig);
        let after = |flag: &str| {
            let at = argv.iter().position(|arg| arg == flag);
            at.map(|at| argv[at + 1].as_str())
        };
        assert_eq!(after("--permission-prompts"), Some("none"), "{mode:?}");
        assert_eq!(after("--permission-mode"), permission_mode, "{mode:?}");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(rig.held_is_empty().await, "{mode:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_can_use_tool_is_gone() {
    let rig = Rig::new("ask-cancel").await;
    let mut rx = rig.session().subscribe_events();
    let turn = start(&rig, "ask-cancel", PlannerPermissionMode::Ask).await;
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
    let mut rx = rig.session().subscribe_events();
    let turn = start(&rig, "ask-exit", PlannerPermissionMode::Ask).await;
    let responder = expect_open(rig.next_held().await, &turn);
    until_completed(&mut rx).await;
    expect_connection_lost(rig.next_held().await, &turn);
    drop(responder);
}
