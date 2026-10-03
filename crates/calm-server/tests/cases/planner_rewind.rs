//! `POST /api/cards/{id}/planner/rewind` on a Codex Planner (#1923): the latest turn leaves the
//! transcript, its input comes back, and the provider drops it when the next turn starts.

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::harness::{HarnessState, Observation};
use serde_json::{Value, json};

use crate::support::planner_queue_fixture::{
    Boot, Issuance, SEED_THREAD_ID, boot_with_issuance, get, idle_snapshot, post_input,
    post_input_with_attachments, send_json, upload_png,
};

const TURN_A: &str = "fake-turn-0001";
const TURN_B: &str = "fake-turn-0002";
const TURN_C: &str = "fake-turn-0003";

async fn rewind(boot: &Boot, turn_id: &str) -> (StatusCode, Value) {
    send_json(
        boot.app.clone(),
        "POST",
        format!(
            "/api/cards/{}/planner/rewind",
            boot.planner_card.id.as_str()
        ),
        "user",
        json!({ "turn_id": turn_id }),
    )
    .await
}

async fn wait_for<F, Fut>(what: &str, mut done: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `(id, turn_id, item_uuid, method)` of every transcript row the card's read route serves.
async fn rows(boot: &Boot) -> Vec<(i64, Option<String>, Option<String>, String)> {
    let (status, rows) = get(
        boot.app.clone(),
        format!("/api/cards/{}/harness/items", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={rows}");
    rows.as_array()
        .expect("an array of rows")
        .iter()
        .map(|row| {
            let text = |key: &str| row[key].as_str().map(str::to_string);
            (
                row["id"].as_i64().unwrap(),
                text("turn_id"),
                text("item_uuid"),
                text("method").unwrap(),
            )
        })
        .collect()
}

async fn stored_snapshot(boot: &Boot) -> Value {
    boot.repo
        .session_projection_by_id(&boot.worker_session_id)
        .await
        .unwrap()
        .expect("the worker session row")
        .handle_state_json
        .expect("a persisted snapshot")
}

fn emit_item(boot: &Boot, turn_id: &str, item: Value) {
    boot.daemon.emit_notification_for_test(Notification::Item {
        method: "item/completed".into(),
        params: json!({ "threadId": SEED_THREAD_ID, "turn": { "id": turn_id }, "item": item }),
    });
}

/// Codex's echo of a user message the drain or a steer projected under `client_id`.
fn echo_user(boot: &Boot, turn_id: &str, client_id: &str, text: &str) {
    emit_item(
        boot,
        turn_id,
        json!({
            "id": format!("codex-user-{client_id}"),
            "clientId": client_id,
            "type": "userMessage",
            "content": [{ "type": "text", "text": format!("User says:\n{text}") }],
        }),
    );
}

/// Start the next turn with `text` and wait until it runs; returns its turn id and client id.
async fn start_turn(boot: &Boot, text: &str, attachments: &[String]) -> (String, String) {
    let started = boot.daemon.turn_start_count_for_test();
    let (status, body) = post_input_with_attachments(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        text,
        attachments,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    wait_for("the turn to run", || async {
        boot.daemon.turn_start_count_for_test() > started
            && matches!(
                boot.harness.state_for_test().await,
                HarnessState::TurnRunning { .. }
            )
    })
    .await;
    let turn_id = format!("fake-turn-{:04}", started + 1);
    let client_id = boot.daemon.started_turn_client_ids_for_test()[started as usize]
        .clone()
        .expect("the drain sends its projection key");
    (turn_id, client_id)
}

/// Codex's side of a finished turn: the echo, one reply, the completion, and the active-turn
/// cache emptied as the daemon's own `turn/completed` handling does.
async fn finish_turn(boot: &Boot, turn_id: &str, client_id: &str, text: &str) {
    echo_user(boot, turn_id, client_id, text);
    emit_item(
        boot,
        turn_id,
        json!({ "id": format!("reply-{turn_id}"), "type": "agentMessage", "text": "ok" }),
    );
    boot.daemon.clear_active_turn_for_test(SEED_THREAD_ID);
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: SEED_THREAD_ID.to_string(),
            turn: json!({ "id": turn_id, "status": "completed" }),
        });
    wait_for("the completion", || async {
        boot.harness.state_for_test().await
            == HarnessState::TurnCompleted {
                last_turn_id: turn_id.to_string(),
            }
    })
    .await;
}

async fn run_turn(boot: &Boot, text: &str) -> String {
    let (turn_id, client_id) = start_turn(boot, text, &[]).await;
    finish_turn(boot, &turn_id, &client_id, text).await;
    turn_id
}

/// Two finished turns, A then B.
async fn two_turns() -> Boot {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    assert_eq!(run_turn(&boot, "first").await, TURN_A);
    assert_eq!(run_turn(&boot, "second").await, TURN_B);
    boot
}

fn turns_of(rows: &[(i64, Option<String>, Option<String>, String)]) -> Vec<Option<String>> {
    rows.iter().map(|(_, turn, _, _)| turn.clone()).collect()
}

#[tokio::test]
async fn rewinding_the_latest_turn_removes_it_and_reverts_codex_before_the_next_turn() {
    let boot = two_turns().await;
    let before = rows(&boot).await;
    assert!(turns_of(&before).contains(&Some(TURN_B.into())));

    let (status, body) = rewind(&boot, TURN_B).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["turn_id"], json!(TURN_B));
    assert_eq!(body["card_id"], json!(boot.planner_card.id.as_str()));
    assert_eq!(
        body["input"],
        json!([{ "presentation": "user", "text": "User says:\nsecond", "attachments": [] }])
    );
    let after = rows(&boot).await;
    assert!(
        after
            .iter()
            .all(|(_, turn, _, _)| turn.as_deref() == Some(TURN_A)),
        "only A's rows stay: {after:?}"
    );
    assert_eq!(
        after,
        before
            .iter()
            .filter(|(_, turn, _, _)| turn.as_deref() == Some(TURN_A))
            .cloned()
            .collect::<Vec<_>>(),
        "A's rows are untouched"
    );
    let events = boot.event_payloads("harness.transcript.rewound").await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["turn_id"], json!(TURN_B));
    assert_eq!(
        events[0]["worker_session_id"],
        json!(boot.worker_session_id)
    );
    assert_eq!(
        events[0]["removed_item_count"],
        json!((before.len() - after.len()) as i64)
    );
    let stored = stored_snapshot(&boot).await;
    assert_eq!(
        stored["pending_rewind"],
        json!({ "provider": "codex", "before_turn_id": TURN_B })
    );
    assert_eq!(stored["last_turn_id"], json!(TURN_A));
    assert_eq!(stored["phase"], json!("turn_completed"));
    assert!(
        boot.daemon.reverted_threads_for_test().is_empty(),
        "applied at the next start only"
    );

    let (status, body) =
        post_input(boot.app.clone(), boot.planner_card.id.as_str(), "edited").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    wait_for("the next turn", || async {
        boot.daemon.turn_start_count_for_test() == 3
    })
    .await;
    assert_eq!(
        boot.daemon.reverted_threads_for_test(),
        vec![(SEED_THREAD_ID.to_string(), TURN_B.to_string(), 2)],
        "one revert of B, after two turn/starts and before the third"
    );
    wait_for("the cut to be consumed", || async {
        stored_snapshot(&boot).await["pending_rewind"].is_null()
    })
    .await;
    assert_eq!(stored_snapshot(&boot).await["last_turn_id"], json!(TURN_C));
}

#[tokio::test]
async fn a_revert_codex_already_applied_still_starts_the_turn_and_clears_the_cut() {
    let boot = two_turns().await;
    let (status, body) = rewind(&boot, TURN_B).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    boot.daemon.answer_revert_turn_not_found_for_test(true);

    let (status, body) =
        post_input(boot.app.clone(), boot.planner_card.id.as_str(), "edited").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    wait_for("the next turn", || async {
        boot.daemon.turn_start_count_for_test() == 3
    })
    .await;
    assert_eq!(boot.daemon.reverted_threads_for_test().len(), 1);
    wait_for("the cut to be consumed", || async {
        stored_snapshot(&boot).await["pending_rewind"].is_null()
    })
    .await;
}

/// Every refusal is a 409 that leaves the rows, the snapshot and the event log as they were.
async fn assert_refused_unchanged(boot: &Boot, turn_id: &str, needle: &str) {
    let rows_before = rows(boot).await;
    let snapshot_before = stored_snapshot(boot).await;
    let (status, body) = rewind(boot, turn_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{needle}: body={body}");
    let message = body["error"].as_str().unwrap_or_default();
    assert!(message.contains(needle), "{needle}: {message}");
    assert!(message.contains("nothing was changed"), "{message}");
    assert_eq!(rows(boot).await, rows_before, "{needle}: rows changed");
    assert_eq!(
        stored_snapshot(boot).await,
        snapshot_before,
        "{needle}: snapshot changed"
    );
}

#[tokio::test]
async fn every_refusal_changes_nothing() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    run_turn(&boot, "first").await;
    let (turn_b, client_b) = start_turn(&boot, "second", &[]).await;
    assert_refused_unchanged(&boot, &turn_b, "still running").await;
    finish_turn(&boot, &turn_b, &client_b, "second").await;

    boot.daemon
        .set_active_turn_for_test(SEED_THREAD_ID, &turn_b);
    assert_refused_unchanged(&boot, &turn_b, "provider still reports a running turn").await;
    boot.daemon.clear_active_turn_for_test(SEED_THREAD_ID);

    assert_refused_unchanged(&boot, TURN_A, "only the latest turn").await;

    // The tx is conditional on this runtime still carrying the card.
    sqlx::query("UPDATE worker_sessions SET state = 'failed' WHERE id = ?1")
        .bind(&boot.worker_session_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let rows_before = rows(&boot).await;
    let error = boot
        .harness
        .rewind_turn(TURN_B.into())
        .await
        .expect_err("a retired runtime rewinds nothing");
    assert!(
        error
            .to_string()
            .contains("no longer the card's active session"),
        "{error}"
    );
    assert_eq!(rows(&boot).await, rows_before);
    assert!(
        boot.event_payloads("harness.transcript.rewound")
            .await
            .is_empty()
    );
    sqlx::query("UPDATE worker_sessions SET state = 'idle' WHERE id = ?1")
        .bind(&boot.worker_session_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();

    let (status, body) = rewind(&boot, TURN_B).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_refused_unchanged(&boot, TURN_A, "send the edited message first").await;

    // Last: a paused queue keeps the message waiting.
    boot.harness.pause_issuance_for_dev();
    let (status, body) = post_input(boot.app.clone(), boot.planner_card.id.as_str(), "wait").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_refused_unchanged(&boot, TURN_A, "still waiting to be sent").await;
}

/// A turn the kernel started for a system update carries a non-user segment.
#[tokio::test]
async fn a_turn_that_carried_a_system_update_is_refused() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    run_turn(&boot, "first").await;
    boot.harness
        .observe(Observation::SystemContext {
            text: "a worker finished".into(),
        })
        .unwrap();
    wait_for("the system turn", || async {
        boot.daemon.turn_start_count_for_test() == 2
            && matches!(
                boot.harness.state_for_test().await,
                HarnessState::TurnRunning { .. }
            )
    })
    .await;
    let client = boot.daemon.started_turn_client_ids_for_test()[1]
        .clone()
        .unwrap();
    finish_turn(&boot, TURN_B, &client, "a worker finished").await;
    assert_refused_unchanged(&boot, TURN_B, "system update").await;
}

/// A user message codex recorded without the kernel's projection has no input to put back.
#[tokio::test]
async fn a_user_message_without_its_input_is_refused() {
    let boot = two_turns().await;
    emit_item(
        &boot,
        TURN_B,
        json!({
            "id": "unprojected",
            "type": "userMessage",
            "content": [{ "type": "text", "text": "typed elsewhere" }],
        }),
    );
    wait_for("the unprojected row", || async {
        rows(&boot)
            .await
            .iter()
            .any(|(_, _, uuid, _)| uuid.as_deref() == Some("unprojected"))
    })
    .await;
    assert_refused_unchanged(&boot, TURN_B, "without its text").await;
}

/// A late row of A after B's rows: the suffix above the boundary no longer holds all of B.
#[tokio::test]
async fn a_row_of_the_turn_below_the_boundary_is_refused() {
    let boot = two_turns().await;
    emit_item(
        &boot,
        TURN_A,
        json!({ "id": "late-a", "type": "agentMessage", "text": "late" }),
    );
    wait_for("the late row", || async {
        rows(&boot)
            .await
            .iter()
            .any(|(_, _, uuid, _)| uuid.as_deref() == Some("late-a"))
    })
    .await;
    assert_refused_unchanged(&boot, TURN_B, "recorded inside this turn").await;
}

#[tokio::test]
async fn a_late_frame_of_the_removed_turn_writes_no_row() {
    let boot = two_turns().await;
    let (status, body) = rewind(&boot, TURN_B).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    emit_item(
        &boot,
        TURN_B,
        json!({ "id": "late-b", "type": "agentMessage", "text": "late" }),
    );
    boot.daemon.emit_notification_for_test(Notification::Other {
        method: "turn/plan/updated".into(),
        params: json!({ "threadId": SEED_THREAD_ID, "turnId": TURN_B, "plan": [] }),
    });
    // Frames are handled in order, so once this one has a row the two before it were handled.
    emit_item(
        &boot,
        TURN_A,
        json!({ "id": "sentinel", "type": "agentMessage", "text": "sentinel" }),
    );
    wait_for("the sentinel row", || async {
        rows(&boot)
            .await
            .iter()
            .any(|(_, _, uuid, _)| uuid.as_deref() == Some("sentinel"))
    })
    .await;
    assert!(
        !turns_of(&rows(&boot).await).contains(&Some(TURN_B.into())),
        "{:?}",
        rows(&boot).await
    );
}

#[tokio::test]
async fn an_image_sent_again_by_a_steer_comes_back_once() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    run_turn(&boot, "first").await;
    let (status, uploaded) = upload_png(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        b"rewind-image",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={uploaded}");
    let image = uploaded["attachmentId"].as_str().unwrap().to_string();
    let (turn_b, client_b) = start_turn(&boot, "look", std::slice::from_ref(&image)).await;
    let (status, posted) = post_input_with_attachments(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "look again",
        std::slice::from_ref(&image),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={posted}");
    let entry = posted["entry_id"].as_str().unwrap().to_string();
    let (status, body) = send_json(
        boot.app.clone(),
        "POST",
        format!(
            "/api/cards/{}/planner/input/{entry}/steer",
            boot.planner_card.id.as_str()
        ),
        "user",
        json!({ "if_entry_rev": 0 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    echo_user(&boot, &turn_b, &entry, "look again");
    finish_turn(&boot, &turn_b, &client_b, "look").await;

    let (status, body) = rewind(&boot, &turn_b).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 2, "prompt then steer: {body}");
    assert_eq!(input[0]["text"], json!("User says:\nlook"));
    assert_eq!(input[1]["text"], json!("User says:\nlook again"));
    let ids = input
        .iter()
        .flat_map(|segment| segment["attachments"].as_array().unwrap().iter())
        .map(|attachment| attachment["id"].clone())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 1, "named once: {body}");
    assert!(
        ids[0].as_str().unwrap().starts_with(&image),
        "{ids:?} vs {image}"
    );
}

#[tokio::test]
async fn a_conversation_with_no_live_harness_is_dormant() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Paused).await;
    boot.harness
        .observe(Observation::TrackGoal { text: "x".into() })
        .unwrap();
    boot.harness.shutdown().await.unwrap();
    sqlx::query("UPDATE worker_sessions SET state = 'failed' WHERE id = ?1")
        .bind(&boot.worker_session_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let (status, body) = rewind(&boot, TURN_A).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("planner_harness_dormant"));
}
