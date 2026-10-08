//! #2348 A5: a managed ACP Planner's permission requests under the `ask` mode, through the
//! production boot, routes and harness. The fake agent sends OpenCode's request shape; the test
//! taps the held-request messages the turn pushes, in order, without changing them.
use super::*;
use calm_server::acp_planner::test_seams::{HeldNote, observe_held, pause_after_fence};
use calm_server::event::{AskDelivery, AskQuestion, Event};

fn permission_log(root: &Root) -> Vec<Value> {
    std::fs::read_to_string(root.path().join("permission-log.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The `OPENCODE_PERMISSION` each Planner (not readiness) launch of the agent carried.
pub(super) fn launch_permissions(root: &Root) -> Vec<Value> {
    std::fs::read_to_string(root.path().join("environment.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|launch| launch["readiness"] == false)
        .map(|launch| launch["opencode_permission"].clone())
        .collect()
}

async fn wait_log(root: &Root, n: usize) -> Vec<Value> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let log = permission_log(root);
            if log.len() >= n {
                return log;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the agent logged its permission replies")
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
            title: "Run: echo one > one.txt".into(),
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
    assert_eq!(
        launch_permissions(&root),
        vec![
            json!(r#"{"bash":"ask","edit":"ask","webfetch":"ask","external_directory":"allow"}"#);
            3
        ],
        "every ask turn's agent asks before bash, edits and fetches"
    );
    stack.shutdown().await;
}

/// The turn settles on its own while its request still waits. Its read's end answers the request
/// `cancelled` before anything else: an answer the person sends through the route afterwards,
/// while the agent process still runs, never reaches the agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_an_answer_after_the_turn_settled_never_reaches_the_agent() {
    let root = Root::new("unused");
    let (stack, track, card, _notes) = ask_track(&root, "ask-settle").await;
    let pause = pause_after_fence(&stack.runtime(&card).await.id);
    let (status, body) = stack.post_input(&card, "use a tool").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let asked = wait_rows(&stack, &track, "ask.requested", 1).await;
    let ask_id = hold_ask(&asked[0]);
    tokio::time::timeout(Duration::from_secs(20), pause.entered.notified())
        .await
        .expect("the turn's read ended");
    assert_eq!(
        wait_log(&root, 1).await,
        [json!({"event": "reply", "outcome": {"outcome": "cancelled"}})]
    );
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/tracks/{track}/asks/{ask_id}/answer"),
            Some(json!({"answers": [{"option": 0}]})),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    // Room for a late write to land while the agent still reads; the fence means none comes.
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(pause);
    assert_eq!(
        stack.wait_outcomes(&card, 1).await[0]["status"],
        "completed"
    );
    assert_eq!(
        permission_log(&root),
        [json!({"event": "reply", "outcome": {"outcome": "cancelled"}})],
        "no answer after the fence"
    );
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

/// #2441: a card the route set to `full` launches its agent allowing each acting tool by name, and
/// a permission request that still arrives is answered `cancelled` as under `never`: no ask, and
/// nothing reaches the held-request channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_full_mode_allows_the_agents_tools_and_refuses_a_stray_request() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    let (status, body) = stack
        .send(
            "PUT",
            &format!("/api/cards/{card}/planner/permission-mode"),
            Some(json!({"permission_mode": "full"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    std::fs::write(root.path().join("scenario"), "permission").unwrap();
    let mut held = observe_held(&stack.runtime(&card).await.id);
    assert_eq!(
        turn(&stack, &card, "request permission", 1).await["status"],
        "interrupted"
    );
    let [permission] = launch_permissions(&root).try_into().expect("one launch");
    let permission: Value = serde_json::from_str(permission.as_str().unwrap()).unwrap();
    assert_eq!(
        permission,
        json!({"bash": "allow", "edit": "allow", "webfetch": "allow",
            "external_directory": "allow", "doom_loop": "allow", "read": "allow"})
    );
    wait_file(&root, "permission-reply.json").await;
    let reply: Value = serde_json::from_str(
        &std::fs::read_to_string(root.path().join("permission-reply.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(reply["result"]["outcome"], json!({"outcome": "cancelled"}));
    assert!(held.try_recv().is_err(), "a full turn holds nothing");
    assert!(rows(&stack, &track, "ask.requested").await.is_empty());
    stack.shutdown().await;
}
