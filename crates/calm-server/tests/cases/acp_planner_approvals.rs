//! #2348 A5: a managed ACP Planner's permission requests under the `ask` mode, through the
//! production boot, routes and harness. The fake agent sends OpenCode's request shape; the test
//! taps the held-request messages the turn pushes, in order, without changing them.
use super::*;
use calm_server::acp_planner::test_seams::{HeldNote, observe_held};
use calm_server::event::{AskDelivery, AskQuestion, Event};

fn permission_log(root: &Root) -> Vec<Value> {
    std::fs::read_to_string(root.path().join("permission-log.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn rows(stack: &Stack, track: &str, kind: &str) -> Vec<(i64, Event)> {
    stack
        .repo()
        .events_for_track(track, &[kind], None)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.id, row.event))
        .collect()
}

async fn wait_rows(stack: &Stack, track: &str, kind: &str, n: usize) -> Vec<(i64, Event)> {
    for _ in 0..400 {
        let found = rows(stack, track, kind).await;
        if found.len() >= n {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{kind} never reached {n} rows");
}

async fn next_note(notes: &mut tokio::sync::mpsc::UnboundedReceiver<HeldNote>) -> HeldNote {
    tokio::time::timeout(Duration::from_secs(20), notes.recv())
        .await
        .expect("a held-request message in time")
        .expect("tap open")
}

/// An ACP Planner track whose card the permission-mode route set to `ask`, with its turns'
/// held-request messages tapped.
async fn ask_track(
    root: &Root,
    scenario: &str,
) -> (
    Stack,
    String,
    String,
    tokio::sync::mpsc::UnboundedReceiver<HeldNote>,
) {
    let stack = boot(root).await;
    let (track, card) = create(&stack).await;
    let (status, body) = stack
        .send(
            "PUT",
            &format!("/api/cards/{card}/planner/permission-mode"),
            Some(json!({"permission_mode": "ask"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    std::fs::write(root.path().join("scenario"), scenario).unwrap();
    let notes = observe_held(&stack.runtime(&card).await.id);
    (stack, track, card, notes)
}

/// The ask's question is the agent's tool call and its options, by name, in its order.
fn hold_ask(asked: &(i64, Event)) -> i64 {
    let (
        ask_id,
        Event::AskRequested {
            questions,
            delivery,
            ..
        },
    ) = asked
    else {
        panic!("{asked:?}");
    };
    assert_eq!(*delivery, AskDelivery::Hold);
    assert_eq!(
        questions,
        &vec![AskQuestion {
            title: "execute: echo one > one.txt".into(),
            options: vec!["Allow once".into(), "Always allow".into(), "Reject".into()],
        }]
    );
    *ask_id
}

/// Each option the person picks reaches the agent as exactly that option's id, and the turn goes
/// on to complete; nothing is withdrawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_ask_mode_holds_each_request_and_answers_the_chosen_option_id() {
    let root = Root::new("unused");
    let (stack, track, card, mut notes) = ask_track(&root, "ask").await;
    for (n, option_id) in ["once", "always", "reject"].into_iter().enumerate() {
        let (status, body) = stack.post_input(&card, "use a tool").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let asked = wait_rows(&stack, &track, "ask.requested", n + 1).await;
        let ask_id = hold_ask(&asked[n]);
        let HeldNote::Open { connection, .. } = next_note(&mut notes).await else {
            panic!("expected Open first");
        };
        let (status, body) = stack
            .send(
                "POST",
                &format!("/api/tracks/{track}/asks/{ask_id}/answer"),
                Some(json!({"answers": [{"option": n}]})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let outcome = stack.wait_outcomes(&card, n + 1).await[n].clone();
        assert_eq!(outcome["status"], "completed", "{outcome}");
        assert_eq!(
            next_note(&mut notes).await,
            HeldNote::ConnectionLost { connection }
        );
        assert_eq!(
            permission_log(&root)[n],
            json!({"event": "reply", "outcome": {"outcome": "selected", "optionId": option_id}})
        );
        stack
            .wait_phase(&stack.runtime(&card).await.id, "turn_completed")
            .await;
    }
    assert!(rows(&stack, &track, "ask.withdrawn").await.is_empty());
    stack.shutdown().await;
}

/// Stopping the turn while the request waits sends `session/cancel`, then answers the request
/// `cancelled`; the request is gone and its ask withdrawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_interrupt_answers_a_held_request_cancelled_and_withdraws_its_ask() {
    let root = Root::new("unused");
    let (stack, track, card, mut notes) = ask_track(&root, "ask-cancel").await;
    let (status, body) = stack.post_input(&card, "use a tool").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let asked = wait_rows(&stack, &track, "ask.requested", 1).await;
    let ask_id = hold_ask(&asked[0]);
    let HeldNote::Open {
        request_key,
        connection,
    } = next_note(&mut notes).await
    else {
        panic!("expected Open first");
    };
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/interrupt"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(next_note(&mut notes).await, HeldNote::Gone { request_key });
    let withdrawn = wait_rows(&stack, &track, "ask.withdrawn", 1).await;
    assert!(
        matches!(&withdrawn[0].1, Event::AskWithdrawn { ask_id: id, .. } if *id == ask_id),
        "{withdrawn:?}"
    );
    let outcome = stack.wait_outcomes(&card, 1).await[0].clone();
    assert_eq!(outcome["status"], "interrupted", "{outcome}");
    assert_eq!(
        permission_log(&root),
        [
            json!({"event": "cancel"}),
            json!({"event": "reply", "outcome": {"outcome": "cancelled"}})
        ]
    );
    assert_eq!(
        next_note(&mut notes).await,
        HeldNote::ConnectionLost { connection }
    );
    stack.shutdown().await;
}

/// The agent process exits while its request waits: the request was opened before its connection
/// was lost, and the ask is withdrawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_process_exit_withdraws_a_held_request_after_opening_it() {
    let root = Root::new("unused");
    let (stack, track, card, mut notes) = ask_track(&root, "ask-exit").await;
    let (status, body) = stack.post_input(&card, "use a tool").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let HeldNote::Open { connection, .. } = next_note(&mut notes).await else {
        panic!("expected Open first");
    };
    assert_eq!(
        next_note(&mut notes).await,
        HeldNote::ConnectionLost { connection }
    );
    let asked = wait_rows(&stack, &track, "ask.requested", 1).await;
    let ask_id = hold_ask(&asked[0]);
    let withdrawn = wait_rows(&stack, &track, "ask.withdrawn", 1).await;
    assert!(
        matches!(&withdrawn[0].1, Event::AskWithdrawn { ask_id: id, .. } if *id == ask_id),
        "{withdrawn:?}"
    );
    assert_eq!(stack.wait_outcomes(&card, 1).await[0]["status"], "failed");
    stack.shutdown().await;
}
