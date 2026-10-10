//! `POST /api/cards/{id}/planner/input` with `replaces_turn` on a Codex Planner (#1923, #2043): the
//! latest turn leaves the transcript and the new message is queued in one commit that binds the
//! send's key, and the provider drops the turn when the next turn starts.

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::harness::{HarnessState, Observation, SendKey};
use serde_json::{Value, json};

use super::planner_harness_live_replies::ITEM_TABLE;
use crate::support::planner_queue_fixture::{
    Boot, Issuance, SEED_THREAD_ID, boot_with_issuance, get, idle_snapshot, post_input_keyed,
    post_input_keyed_as, post_input_with_attachments, send_json, upload_png,
};

const TURN_A: &str = "fake-turn-0001";
pub(super) const TURN_B: &str = "fake-turn-0002";
const TURN_C: &str = "fake-turn-0003";

/// One replace under `key`, as `actor`.
async fn replace_keyed_as(
    boot: &Boot,
    actor: &str,
    turn_id: &str,
    text: &str,
    key: &str,
) -> (StatusCode, Value) {
    post_input_keyed_as(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        json!({ "text": text, "replaces_turn": turn_id }),
        key,
        actor,
    )
    .await
}

/// One replace as a new send: a fresh key.
pub(super) async fn replace(boot: &Boot, turn_id: &str, text: &str) -> (StatusCode, Value) {
    replace_keyed_as(boot, "user", turn_id, text, &calm_server::model::new_id()).await
}

/// `(idempotency_key, entry_id)` of every key bound on the card.
async fn bindings(boot: &Boot) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT idempotency_key, entry_id FROM planner_input_idempotency WHERE card_id = ?1 \
         ORDER BY created_at_ms, idempotency_key",
    )
    .bind(boot.planner_card.id.as_str())
    .fetch_all(boot.repo.pool())
    .await
    .unwrap()
}

/// The entry ids `GET /planner/run` lists as waiting.
async fn pending_ids(boot: &Boot) -> Vec<String> {
    let (status, run) = get(
        boot.app.clone(),
        format!("/api/cards/{}/planner/run", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    run["pending"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| entry["entry_id"].as_str().unwrap().to_string())
        .collect()
}

pub(super) async fn wait_for<F, Fut>(what: &str, mut done: F)
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

// The run loop publishes its in-memory phase before committing the snapshot. Tests that
// compare persisted state must wait for that commit, including the matching turn identity.
pub(super) async fn wait_for_stored_phase(boot: &Boot, phase: &str, turn_id: &str) {
    wait_for("the persisted turn phase", || async {
        let snapshot = stored_snapshot(boot).await;
        snapshot["phase"] == phase && snapshot["last_turn_id"] == turn_id
    })
    .await;
}

pub(super) fn emit_item(boot: &Boot, turn_id: &str, item: Value) {
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
    wait_for_stored_phase(boot, "turn_running", &turn_id).await;
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
    wait_for_stored_phase(boot, "turn_completed", turn_id).await;
}

async fn run_turn(boot: &Boot, text: &str) -> String {
    let (turn_id, client_id) = start_turn(boot, text, &[]).await;
    finish_turn(boot, &turn_id, &client_id, text).await;
    turn_id
}

/// Two finished turns, A then B.
pub(super) async fn two_turns() -> Boot {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    assert_eq!(run_turn(&boot, "first").await, TURN_A);
    assert_eq!(run_turn(&boot, "second").await, TURN_B);
    boot
}

fn turns_of(rows: &[(i64, Option<String>, Option<String>, String)]) -> Vec<Option<String>> {
    rows.iter().map(|(_, turn, _, _)| turn.clone()).collect()
}

/// Whether the `index`th `turn/start` carried `text`.
fn started_with(boot: &Boot, index: usize, text: &str) -> bool {
    boot.daemon
        .started_turns_for_test()
        .get(index)
        .is_some_and(|(_, input)| format!("{input:?}").contains(text))
}

#[tokio::test]
async fn a_replace_removes_the_turn_and_its_message_reverts_codex_before_starting() {
    let boot = two_turns().await;
    let before = rows(&boot).await;
    assert!(turns_of(&before).contains(&Some(TURN_B.into())));

    let (status, body) = replace(&boot, TURN_B, "edited").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["card_id"], json!(boot.planner_card.id.as_str()));
    assert_eq!(body["worker_session_id"], json!(boot.worker_session_id));
    assert!(body["entry_id"].is_string(), "{body}");
    let events = boot.event_payloads("harness.transcript.rewound").await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["turn_id"], json!(TURN_B));
    assert_eq!(
        events[0]["worker_session_id"],
        json!(boot.worker_session_id)
    );
    let removed = before
        .iter()
        .filter(|(_, turn, _, _)| turn.as_deref() == Some(TURN_B))
        .count();
    assert!(removed > 0);
    assert!(
        events[0]["removed_item_count"].as_i64().unwrap() >= removed as i64,
        "{events:?}"
    );

    wait_for("the next turn", || async {
        boot.daemon.turn_start_count_for_test() == 3
    })
    .await;
    assert_eq!(
        boot.daemon.reverted_threads_for_test(),
        vec![(SEED_THREAD_ID.to_string(), TURN_B.to_string(), 2)],
        "one revert of B, after two turn/starts and before the third"
    );
    assert!(
        started_with(&boot, 2, "edited"),
        "the edit is the next turn"
    );
    let after = rows(&boot).await;
    assert!(
        !turns_of(&after).contains(&Some(TURN_B.into())),
        "B's rows are gone: {after:?}"
    );
    assert_eq!(
        after
            .iter()
            .filter(|(_, turn, _, _)| turn.as_deref() == Some(TURN_A))
            .cloned()
            .collect::<Vec<_>>(),
        before
            .iter()
            .filter(|(_, turn, _, _)| turn.as_deref() == Some(TURN_A))
            .cloned()
            .collect::<Vec<_>>(),
        "A's rows are untouched"
    );
    wait_for("the cut to be consumed", || async {
        stored_snapshot(&boot).await["pending_rewind"].is_null()
    })
    .await;
    assert_eq!(stored_snapshot(&boot).await["last_turn_id"], json!(TURN_C));
}

/// Codex answers `turn not found` alike for a wrong turn and for one it already reverted (#2512),
/// so that answer is a refusal: no turn starts and the cut stays.
#[tokio::test]
async fn a_revert_codex_answers_turn_not_found_is_refused_and_keeps_the_cut() {
    let boot = two_turns().await;
    boot.daemon.answer_revert_turn_not_found_for_test(true);
    let (status, body) = replace(&boot, TURN_B, "edited").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    wait_for("the refusal", || async {
        boot.harness.refused_issuances_for_test() == 1
    })
    .await;
    assert_eq!(boot.daemon.reverted_threads_for_test().len(), 1);
    assert_eq!(boot.daemon.turn_start_count_for_test(), 2);
    assert_eq!(
        stored_snapshot(&boot).await["pending_rewind"],
        json!({ "provider": "codex", "before_turn_id": TURN_B })
    );
}

/// A revert that went through is the kernel's own record (#2512): when `turn/start` then fails, the
/// retry starts the turn without reverting again.
#[tokio::test]
async fn a_turn_start_that_fails_after_the_revert_retries_without_reverting_again() {
    let boot = two_turns().await;
    boot.daemon.fail_turn_start_for_test();
    let (status, body) = replace(&boot, TURN_B, "edited").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    wait_for("the refusal", || async {
        boot.harness.refused_issuances_for_test() == 1
    })
    .await;
    assert_eq!(
        stored_snapshot(&boot).await["pending_rewind"],
        json!({ "provider": "codex", "before_turn_id": TURN_B, "reverted": true })
    );
    boot.daemon.clear_turn_start_failure_for_test();
    boot.harness.retry_issuance_now().await;
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

/// The record of a revert that went through is durable: a harness restarted from the snapshot a
/// failed start left starts the queued turn without reverting again (#2512).
#[tokio::test]
async fn a_recorded_revert_survives_a_restart() {
    let boot = two_turns().await;
    boot.daemon.fail_turn_start_for_test();
    let (status, body) = replace(&boot, TURN_B, "edited").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    wait_for("the refusal", || async {
        boot.harness.refused_issuances_for_test() == 1
    })
    .await;
    let stored = stored_snapshot(&boot).await;
    let restarted = boot_with_issuance(
        calm_server::harness::HarnessSnapshot::from_value_strict(stored),
        Issuance::Live,
    )
    .await;
    wait_for("the queued turn", || async {
        restarted.daemon.turn_start_count_for_test() == 1
    })
    .await;
    assert!(restarted.daemon.reverted_threads_for_test().is_empty());
    let started = restarted.daemon.started_turns_for_test();
    assert!(format!("{started:?}").contains("edited"), "{started:?}");
}

/// The removal, the message and the key are one commit: the snapshot that holds the cut holds the
/// message, and the key is bound to the entry it answered with.
#[tokio::test]
async fn the_cut_the_message_and_the_key_commit_together() {
    let boot = two_turns().await;
    boot.harness.pause_issuance_for_dev();
    let key = calm_server::model::new_id();
    let (status, body) = replace_keyed_as(&boot, "user", TURN_B, "edited", &key).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let entry = body["entry_id"].as_str().unwrap().to_string();

    let stored = stored_snapshot(&boot).await;
    assert_eq!(
        stored["pending_rewind"],
        json!({ "provider": "codex", "before_turn_id": TURN_B })
    );
    assert_eq!(stored["last_turn_id"], json!(TURN_A));
    assert_eq!(stored["phase"], json!("turn_completed"));
    let queue = stored["pending_queue"].as_array().expect("a queue");
    assert_eq!(queue.len(), 1, "{stored}");
    assert!(queue[0].to_string().contains("edited"), "{stored}");
    assert_eq!(pending_ids(&boot).await, vec![entry.clone()]);
    let bound = bindings(&boot).await;
    assert_eq!(bound.len(), 3, "two plain sends and the replace: {bound:?}");
    assert!(bound.contains(&(key.clone(), Some(entry))), "{bound:?}");
    assert!(!turns_of(&rows(&boot).await).contains(&Some(TURN_B.into())));
    assert!(
        boot.daemon.reverted_threads_for_test().is_empty(),
        "applied at the next start only"
    );
}

/// A lost answer: the same key again replays the first answer, with no second removal and no
/// second message, wherever the message has gone since.
#[tokio::test]
async fn a_replace_sent_again_under_its_key_replays_without_a_second_rewind_or_message() {
    let boot = two_turns().await;
    let key = calm_server::model::new_id();
    let (status, first) = replace_keyed_as(&boot, "user", TURN_B, "edited", &key).await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    wait_for("the next turn", || async {
        boot.daemon.turn_start_count_for_test() == 3
    })
    .await;

    let (status, again) = replace_keyed_as(&boot, "user", TURN_B, "edited", &key).await;
    assert_eq!(status, StatusCode::OK, "body={again}");
    assert_eq!(again, first, "the first answer, replayed");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        boot.daemon.turn_start_count_for_test(),
        3,
        "no second message"
    );
    assert_eq!(boot.daemon.reverted_threads_for_test().len(), 1);
    assert_eq!(
        boot.event_payloads("harness.transcript.rewound")
            .await
            .len(),
        1,
        "no second removal"
    );
    assert!(pending_ids(&boot).await.is_empty());
}

/// The key is bound to what it carried: another turn, or the same words as a plain send, is a
/// different message.
#[tokio::test]
async fn a_key_bound_to_one_message_refuses_another() {
    let boot = two_turns().await;
    boot.harness.pause_issuance_for_dev();
    let key = calm_server::model::new_id();
    let (status, body) = replace_keyed_as(&boot, "user", TURN_B, "edited", &key).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    let (status, body) = replace_keyed_as(&boot, "user", TURN_A, "edited", &key).await;
    assert_eq!(status, StatusCode::CONFLICT, "another turn: {body}");
    assert_eq!(body["code"], json!("idempotency_key_reused"));
    let plain = |key: String| {
        let app = boot.app.clone();
        let card = boot.planner_card.id.as_str().to_string();
        async move { post_input_keyed(app, &card, json!({ "text": "edited" }), &key).await }
    };
    let (status, body) = plain(key.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "a plain send: {body}");
    assert_eq!(body["code"], json!("idempotency_key_reused"));

    // And the other way round: a key a plain send bound is not a replace's.
    let plain_key = calm_server::model::new_id();
    let (status, body) = plain(plain_key.clone()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let (status, body) = replace_keyed_as(&boot, "user", TURN_A, "edited", &plain_key).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("idempotency_key_reused"));
}

/// Every refusal is a 409 `planner_turn_not_replaceable` that leaves the rows, the snapshot, the
/// queue and the key bindings as they were.
async fn assert_refused_unchanged(boot: &Boot, turn_id: &str, needle: &str) {
    let rows_before = rows(boot).await;
    let snapshot_before = stored_snapshot(boot).await;
    let bound_before = bindings(boot).await;
    let pending_before = pending_ids(boot).await;
    let (status, body) = replace(boot, turn_id, "edited").await;
    assert_eq!(status, StatusCode::CONFLICT, "{needle}: body={body}");
    assert_eq!(
        body["code"],
        json!("planner_turn_not_replaceable"),
        "{body}"
    );
    let message = body["error"].as_str().unwrap_or_default();
    assert!(message.contains(needle), "{needle}: {message}");
    assert!(message.contains("nothing was changed"), "{message}");
    assert_eq!(rows(boot).await, rows_before, "{needle}: rows changed");
    assert_eq!(
        stored_snapshot(boot).await,
        snapshot_before,
        "{needle}: snapshot changed"
    );
    assert_eq!(
        bindings(boot).await,
        bound_before,
        "{needle}: a key was bound"
    );
    assert_eq!(
        pending_ids(boot).await,
        pending_before,
        "{needle}: queue changed"
    );
}

#[tokio::test]
async fn every_refusal_changes_nothing_and_binds_nothing() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    run_turn(&boot, "first").await;
    let (turn_b, client_b) = start_turn(&boot, "second", &[]).await;
    assert_refused_unchanged(&boot, &turn_b, "still running").await;
    let spared = calm_server::model::new_id();
    let (status, _) = replace_keyed_as(&boot, "user", &turn_b, "edited", &spared).await;
    assert_eq!(status, StatusCode::CONFLICT);
    finish_turn(&boot, &turn_b, &client_b, "second").await;

    boot.daemon
        .set_active_turn_for_test(SEED_THREAD_ID, &turn_b);
    assert_refused_unchanged(&boot, &turn_b, "provider still reports a running turn").await;
    boot.daemon.clear_active_turn_for_test(SEED_THREAD_ID);

    assert_refused_unchanged(&boot, TURN_A, "only the latest turn").await;

    // The tx is conditional on this runtime still carrying the card; its refusal rolls the whole
    // commit back, the message in the queue included.
    sqlx::query("UPDATE worker_sessions SET state = 'failed' WHERE id = ?1")
        .bind(&boot.worker_session_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let rows_before = rows(&boot).await;
    let key = SendKey::unique_for_test();
    let error = boot
        .harness
        .replace_turn_durable(TURN_B.into(), "edited".into(), Vec::new(), key.clone())
        .await
        .expect_err("a retired runtime replaces nothing");
    assert!(
        error
            .to_string()
            .contains("no longer the card's active session"),
        "{error}"
    );
    assert_eq!(error.code(), "planner_turn_not_replaceable");
    assert_eq!(rows(&boot).await, rows_before);
    assert!(
        bindings(&boot)
            .await
            .iter()
            .all(|(bound, _)| *bound != key.idempotency_key)
    );
    assert_eq!(
        boot.harness.snapshot().await.pending_len(),
        0,
        "the queue is put back"
    );
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

    // Last: with issuance paused the replacement waits in the queue. A refused key is not spent.
    boot.harness.pause_issuance_for_dev();
    let (status, body) = replace_keyed_as(&boot, "user", TURN_B, "edited", &spared).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let entry = body["entry_id"].as_str().unwrap().to_string();
    assert_refused_unchanged(&boot, TURN_A, "still waiting to be sent").await;
    // Its reader deletes it: the cut still waits for the next message.
    let (status, body) = send_json(
        boot.app.clone(),
        "DELETE",
        format!(
            "/api/cards/{}/planner/input/{entry}",
            boot.planner_card.id.as_str()
        ),
        "user",
        json!({ "if_entry_rev": 0 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_refused_unchanged(&boot, TURN_A, "send a message first").await;
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
    wait_for_stored_phase(&boot, "turn_running", TURN_B).await;
    let client = boot.daemon.started_turn_client_ids_for_test()[1]
        .clone()
        .unwrap();
    finish_turn(&boot, TURN_B, &client, "a worker finished").await;
    assert_refused_unchanged(&boot, TURN_B, "system update").await;
}

/// Kept on purpose (#2043): only a row's recorded input tells a person's message from a system
/// update, so a user message codex recorded without the kernel's projection is refused.
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
    assert_refused_unchanged(&boot, TURN_B, "without its input").await;
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
    boot.harness.pause_issuance_for_dev();
    let (status, body) = replace(&boot, TURN_B, "edited").await;
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

/// #2209 U2: a late native question of the removed turn asks the user nothing; the same question
/// in a turn still on the transcript does.
#[tokio::test]
async fn a_late_question_of_the_removed_turn_asks_nothing() {
    let boot = two_turns().await;
    boot.harness.pause_issuance_for_dev();
    let (status, body) = replace(&boot, TURN_B, "edited").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let question = |id: &str| {
        json!({ "id": id, "type": "agentMessage", "delivery": "async", "text": "Which?",
            "questions": [{ "title": "Which?", "options": ["A", "B"] }] })
    };
    emit_item(&boot, TURN_B, question("late-question"));
    emit_item(&boot, TURN_A, question("kept-question"));
    // Frames are handled in order, so once the kept question is asked the late one was handled.
    wait_for("the kept question's ask", || async {
        !boot.event_payloads("ask.requested").await.is_empty()
    })
    .await;
    let asked: Vec<Value> = boot.event_payloads("ask.requested").await;
    assert_eq!(
        asked
            .iter()
            .map(|ask| ask["source_item_id"].clone())
            .collect::<Vec<_>>(),
        vec![json!("kept-question")],
        "{asked:?}"
    );
}

/// Deleted on purpose (#2043): the eight-image cap guarded only the input the rewind route handed
/// back as one message. The cut never reads images, and the replacing message is checked as any
/// send is, so a turn whose prompt and steer carried nine images can be replaced.
#[tokio::test]
async fn a_turn_that_carried_more_than_eight_images_can_be_replaced() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    run_turn(&boot, "first").await;
    let mut images = Vec::new();
    for index in 0..9u8 {
        let (status, uploaded) = upload_png(
            boot.app.clone(),
            boot.planner_card.id.as_str(),
            &[b'i', index],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "body={uploaded}");
        images.push(uploaded["attachmentId"].as_str().unwrap().to_string());
    }
    let (turn_b, client_b) = start_turn(&boot, "look", &images[..8]).await;
    let (status, posted) = post_input_with_attachments(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "and this one",
        &images[8..],
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
    echo_user(&boot, &turn_b, &entry, "and this one");
    finish_turn(&boot, &turn_b, &client_b, "look").await;

    let (status, body) = replace(&boot, &turn_b, "look at fewer").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert!(!turns_of(&rows(&boot).await).contains(&Some(turn_b)));
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
    let (status, body) = replace(&boot, TURN_A, "edited").await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("planner_harness_dormant"));
}

/// Replacing a turn deletes the person's own message: an agent actor is refused before the card is
/// even read, a blank turn is no turn (400, not a "not the latest" refusal), and nothing changes.
#[tokio::test]
async fn an_agent_actor_or_a_blank_turn_replaces_nothing() {
    let boot = two_turns().await;
    let rows_before = rows(&boot).await;
    let snapshot_before = stored_snapshot(&boot).await;
    let bound_before = bindings(&boot).await;
    for actor in ["ai:codex", "ai:claude", "ai:planner"] {
        let key = calm_server::model::new_id();
        let (status, body) = replace_keyed_as(&boot, actor, TURN_B, "edited", &key).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{actor}: body={body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("X-Calm-Actor: user"),
            "{actor}: {body}"
        );
    }
    for blank in ["", "  "] {
        let (status, body) = replace(&boot, blank, "edited").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{blank:?}: body={body}");
        assert_eq!(body["code"], json!("bad_request"), "{body}");
    }
    assert_eq!(rows(&boot).await, rows_before);
    assert_eq!(stored_snapshot(&boot).await, snapshot_before);
    assert_eq!(bindings(&boot).await, bound_before);
    assert!(
        boot.event_payloads("harness.transcript.rewound")
            .await
            .is_empty()
    );
}

async fn rename_table(boot: &Boot, from: &str, to: &str) {
    sqlx::query(&format!("ALTER TABLE {from} RENAME TO {to}"))
        .execute(boot.repo.pool())
        .await
        .unwrap();
}

async fn live(boot: &Boot) -> Value {
    let (status, body) = get(
        boot.app.clone(),
        format!("/api/cards/{}/harness/live", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    body
}

/// A partial reply whose row failed to store stays live until the next turn starts; once its turn
/// is replaced, its text goes with it.
#[tokio::test]
async fn a_replace_clears_the_live_text_of_the_removed_turn() {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    let (turn, _client) = start_turn(&boot, "stream something", &[]).await;
    let reply = json!({ "id": "reply-1", "type": "agentMessage", "text": "" });
    boot.daemon.emit_notification_for_test(Notification::Item {
        method: "item/started".into(),
        params: json!({ "threadId": SEED_THREAD_ID, "turnId": turn, "item": reply }),
    });
    boot.daemon.emit_notification_for_test(Notification::Item {
        method: "item/agentMessage/delta".into(),
        params: json!({
            "threadId": SEED_THREAD_ID, "turnId": turn, "itemId": "reply-1", "delta": "Half",
        }),
    });
    let streaming = json!({ "turn_id": turn, "items": [{ "item_id": "reply-1", "text": "Half" }] });
    wait_for("the streamed text", || async {
        live(&boot).await == streaming
    })
    .await;

    rename_table(&boot, ITEM_TABLE, "hidden_rows").await;
    boot.harness.interrupt("test stop".into()).await.unwrap();
    wait_for("the interrupt to go out", || async {
        !boot.daemon.interrupted_turns_for_test().is_empty()
    })
    .await;
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: SEED_THREAD_ID.to_string(),
            turn: json!({ "id": turn, "status": "interrupted" }),
        });
    wait_for("the interrupted completion", || async {
        boot.harness.state_for_test().await
            == HarnessState::TurnCompleted {
                last_turn_id: turn.clone(),
            }
    })
    .await;
    rename_table(&boot, "hidden_rows", ITEM_TABLE).await;
    assert_eq!(
        live(&boot).await,
        streaming,
        "premise: the unstored partial stays live"
    );

    boot.harness.pause_issuance_for_dev();
    let (status, body) = replace(&boot, &turn, "edited").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(live(&boot).await, json!({ "turn_id": null, "items": [] }));
}
