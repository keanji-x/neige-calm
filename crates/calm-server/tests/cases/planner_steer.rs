//! #1625 P3 — `POST /api/cards/{id}/planner/input/{entry_id}/steer`, end to
//! end: a queued message goes into the turn that is running now.
//!
//! Driven through the production route, the production run loop
//! (`HarnessObservationCommand::Steer` → `handle_steer`), the fixture daemon's
//! `turn/steer` arm (which answers like codex against the turn its own
//! `turn/start` recorded) and the production transcript read
//! (`GET /api/cards/{id}/harness/items`), over one sqlite repo.
//!
//! Every test that needs a running turn gets one the way production does: the
//! first message drains into `turn/start` on the 50 ms tick, the fake emits
//! `turn/started`, and the loop moves to `TurnRunning`. Nothing here sets the
//! phase by hand except the refusal matrix, whose subject IS the phase gate.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use calm_server::codex_appserver::{InputItem, Notification};
use calm_server::db::prelude::*;
use calm_server::harness::{HarnessSnapshot, HarnessState, IssuingKind, Observation};
use calm_server::shared_codex_appserver::TurnStartReturnHook;
use serde_json::{Value, json};

use crate::support::planner_queue_fixture::{
    Boot, Issuance, SEED_THREAD_ID, boot_with, boot_with_issuance, get, idle_snapshot, post_input,
    send_json,
};

const FIRST_TURN: &str = "fake-turn-0001";
const NO_ACTIVE_TURN: &str = "turn/steer failed: no active turn to steer (code -32600)";

fn steer_uri(card_id: &str, entry_id: &str) -> String {
    format!("/api/cards/{card_id}/planner/input/{entry_id}/steer")
}

async fn steer(boot: &Boot, entry_id: &str, if_entry_rev: u32) -> (StatusCode, Value) {
    send_json(
        boot.app.clone(),
        "POST",
        steer_uri(boot.planner_card.id.as_str(), entry_id),
        "user",
        json!({"if_entry_rev": if_entry_rev}),
    )
    .await
}

/// Queue one message through the production send route and return its id.
async fn queue_one(boot: &Boot, text: &str) -> String {
    let (status, posted) = post_input(boot.app.clone(), boot.planner_card.id.as_str(), text).await;
    assert_eq!(status, StatusCode::OK, "body={posted}");
    posted["entry_id"]
        .as_str()
        .expect("a live harness acks with an id")
        .to_string()
}

async fn pending(boot: &Boot) -> Vec<Value> {
    let (status, run) = get(
        boot.app.clone(),
        format!("/api/cards/{}/planner/run", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    run["pending"].as_array().cloned().unwrap_or_default()
}

async fn transcript(boot: &Boot) -> Vec<Value> {
    let (status, rows) = get(
        boot.app.clone(),
        format!("/api/cards/{}/harness/items", boot.planner_card.id.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={rows}");
    rows.as_array().cloned().unwrap_or_default()
}

fn user_rows(rows: &[Value]) -> Vec<Value> {
    rows.iter()
        .filter(|row| row["item_type"] == "userMessage")
        .cloned()
        .collect()
}

async fn wait_until<F: FnMut() -> bool>(what: &str, mut done: F) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_turn_running(boot: &Boot) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if matches!(
            boot.harness.state_for_test().await,
            HarnessState::TurnRunning { .. }
        ) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the first turn to be running; state={:?}",
            boot.harness.state_for_test().await
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A live harness with one turn running: "first" drained into `turn/start`
/// and the fake answered `turn/started`.
async fn boot_with_a_running_turn() -> Boot {
    let boot = boot_with_issuance(idle_snapshot(vec![]), Issuance::Live).await;
    queue_one(&boot, "first").await;
    wait_until("the first turn/start", || {
        boot.daemon.turn_start_count_for_test() >= 1
    })
    .await;
    wait_for_turn_running(&boot).await;
    assert_eq!(
        boot.daemon.started_turns_for_test().len(),
        1,
        "premise: exactly one turn has been issued"
    );
    boot
}

fn texts_of(items: &[InputItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            InputItem::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn complete_turn(boot: &Boot, turn_id: &str) {
    end_turn(boot, turn_id, "completed");
}

fn end_turn(boot: &Boot, turn_id: &str, status: &str) {
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: SEED_THREAD_ID.to_string(),
            turn: json!({ "id": turn_id, "status": status }),
        });
}

async fn wait_for_turn_completed(boot: &Boot) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if matches!(
            boot.harness.state_for_test().await,
            HarnessState::TurnCompleted { .. }
        ) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the completion never landed; state={:?}",
            boot.harness.state_for_test().await
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The queue as the snapshot ON DISK lists it — what a restart would read,
/// not the live harness.
async fn persisted_pending_ids(boot: &Boot) -> Vec<String> {
    let stored = boot
        .repo
        .session_projection_by_id(&boot.worker_session_id)
        .await
        .unwrap()
        .expect("the worker session row")
        .handle_state_json
        .expect("a persisted snapshot");
    HarnessSnapshot::from_value_strict(stored)
        .pending_entries()
        .iter()
        .filter_map(|entry| entry.id().map(|id| id.as_str().to_string()))
        .collect()
}

/// Codex's echo of a steered message: `item/started` then `item/completed`,
/// both naming the projection by `clientId` and the running turn — what
/// `record_user_prompt_and_emit_turn_item` emits once the turn's next model
/// request records the pending input.
fn echo_steered(boot: &Boot, entry_id: &str, codex_item_id: &str, text: &str) {
    let item = json!({
        "id": codex_item_id,
        "clientId": entry_id,
        "type": "userMessage",
        "content": [{ "type": "text", "text": format!("User says:\n{text}") }]
    });
    for method in ["item/started", "item/completed"] {
        boot.daemon.emit_notification_for_test(Notification::Item {
            method: method.into(),
            params: json!({
                "threadId": SEED_THREAD_ID,
                "turn": { "id": FIRST_TURN },
                "item": item.clone()
            }),
        });
    }
}

/// Until the transcript lists a row under codex's item id — the projection
/// upgraded in place.
async fn wait_for_upgrade(boot: &Boot, codex_item_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let rows = user_rows(&transcript(boot).await);
        if rows.iter().any(|row| row["item_uuid"] == codex_item_id) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the echo to upgrade the projection: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `(entry_id, change)` of every `harness.queue.changed`, oldest first.
async fn queue_changes(boot: &Boot) -> Vec<(String, String)> {
    boot.event_payloads("harness.queue.changed")
        .await
        .iter()
        .map(|payload| {
            (
                payload["entry_id"].as_str().unwrap_or_default().to_string(),
                payload["change"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

/// The whole promise in one test: the entry leaves the queue, codex is handed
/// it with the running turn as `expectedTurnId` and the entry id as
/// `clientUserMessageId`, the departure is announced, the sentence is on the
/// transcript at once, and codex's echo upgrades that one row rather than
/// adding a second.
#[tokio::test]
async fn a_queued_entry_is_steered_into_the_running_turn_and_its_row_is_upgraded_by_the_echo() {
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, "second, now").await;
    assert_eq!(
        pending(&boot).await.len(),
        1,
        "premise: queued behind the running turn"
    );
    let rows_before = user_rows(&transcript(&boot).await);
    assert_eq!(
        rows_before.len(),
        1,
        "premise: the drain's own row and nothing else"
    );

    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["steered"], json!(true));
    assert_eq!(body["entry_id"], json!(entry_id));
    assert_eq!(body["turn_id"], json!(FIRST_TURN));
    assert_eq!(body["card_id"], json!(boot.planner_card.id.as_str()));
    assert_eq!(body["worker_session_id"], json!(boot.worker_session_id));

    // What codex was handed.
    let steered = boot.daemon.steered_turns_for_test();
    assert_eq!(steered.len(), 1, "{steered:?}");
    let (thread, expected, items, client_id) = &steered[0];
    assert_eq!(thread, SEED_THREAD_ID);
    assert_eq!(
        expected, FIRST_TURN,
        "expectedTurnId is the turn that is running"
    );
    assert_eq!(client_id.as_deref(), Some(entry_id.as_str()));
    assert_eq!(
        serde_json::to_value(items).unwrap(),
        json!([{ "type": "text", "text": "User says:\nsecond, now" }]),
        "the entry's own segment: no diff block, no briefing"
    );
    assert_eq!(
        boot.daemon.turn_start_count_for_test(),
        1,
        "a steer starts no turn"
    );

    // The queue no longer lists it — live, and on disk right now, not at
    // the turn's completion (review round 1, 3c): a restart in between must
    // not re-drain a sentence codex holds.
    assert!(pending(&boot).await.is_empty());
    assert!(
        persisted_pending_ids(&boot).await.is_empty(),
        "the snapshot on disk is persisted without the entry before the 200"
    );
    // And it emptied the queue, so the debounce window is gone with it
    // (§4.5, the departure rule; review round 1, 3a).
    assert!(!boot.harness.debounce_hard_fire_for_test().await);
    assert_eq!(
        boot.harness.debounce_timestamps_set_for_test().await,
        (false, false),
        "an empty queue has no pending window to keep"
    );

    // The departure is announced, with the actor who asked.
    let changes = boot.event_payloads("harness.queue.changed").await;
    assert_eq!(
        changes,
        vec![json!({
            "worker_session_id": boot.worker_session_id,
            "card_id": boot.planner_card.id.as_str(),
            "track_id": boot.planner_card.track_id.as_str(),
            "entry_id": entry_id,
            "change": "steered",
            "actor": {"kind": "User"},
        })]
    );

    // The sentence is on the transcript before any echo, keyed by the entry
    // id, and announced through the per-row event.
    let rows = user_rows(&transcript(&boot).await);
    assert_eq!(
        rows.len(),
        2,
        "the drain's row and the steer's row: {rows:?}"
    );
    let projection = rows
        .iter()
        .find(|row| row["item_uuid"] == entry_id)
        .expect("a row keyed by the steered entry's id");
    assert_eq!(projection["method"], "item/completed");
    assert_eq!(projection["turn_id"], Value::Null);
    let params: Value = serde_json::from_str(projection["params"].as_str().unwrap()).unwrap();
    assert_eq!(params["_projection"], true);
    assert_eq!(params["item"]["clientId"], entry_id);
    assert!(
        projection["input_segments"][0]["text"]
            .as_str()
            .unwrap()
            .contains("second, now")
    );
    let projection_db_id = projection["id"].as_i64().unwrap();
    let added = boot.event_payloads("harness.item.added").await;
    assert!(
        added
            .iter()
            .any(|payload| payload["item_uuid"] == entry_id
                && payload["item_db_id"] == projection_db_id),
        "the steer's row is announced once codex has taken it: {added:?}"
    );

    // Codex's echo: `item/started` then `item/completed`, both naming the
    // projection by `clientId` and the RUNNING turn — what
    // `record_user_prompt_and_emit_turn_item` emits for steered input.
    let echo_item = json!({
        "id": "item-user-codex-2",
        "clientId": entry_id,
        "type": "userMessage",
        "content": [{ "type": "text", "text": "User says:\nsecond, now" }]
    });
    for method in ["item/started", "item/completed"] {
        boot.daemon.emit_notification_for_test(Notification::Item {
            method: method.into(),
            params: json!({
                "threadId": SEED_THREAD_ID,
                "turn": { "id": FIRST_TURN },
                "item": echo_item.clone()
            }),
        });
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let rows = loop {
        let rows = user_rows(&transcript(&boot).await);
        if rows
            .iter()
            .any(|row| row["item_uuid"] == "item-user-codex-2")
        {
            break rows;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the echo to upgrade the projection: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        rows.len(),
        2,
        "still one row per sentence after the echo: {rows:?}"
    );
    let upgraded = rows
        .iter()
        .find(|row| row["item_uuid"] == "item-user-codex-2")
        .unwrap();
    assert_eq!(
        upgraded["id"].as_i64().unwrap(),
        projection_db_id,
        "the same row"
    );
    assert_eq!(upgraded["turn_id"], FIRST_TURN);
    assert_eq!(
        upgraded["input_segments"], projection["input_segments"],
        "the steer's segments survive the upgrade"
    );
    let params: Value = serde_json::from_str(upgraded["params"].as_str().unwrap()).unwrap();
    assert_eq!(params.get("_projection"), None);
}

/// The double-delivery test. Once codex has RECORDED the entry (its echo
/// arrived), the queue must not: after the running turn completes, the tick
/// finds nothing to drain, and a later message starts a turn that carries
/// only itself. The echo is part of the premise since review round 1: a
/// completion with no echo is codex having dropped the input, and the
/// completion sweep restores it on purpose (the tests under "a steer codex
/// accepted, then dropped").
#[tokio::test]
async fn a_steered_entry_is_not_drained_again_when_the_turn_completes() {
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, "steered once").await;
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    echo_steered(&boot, &entry_id, "item-user-codex-3", "steered once");
    wait_for_upgrade(&boot, "item-user-codex-3").await;

    complete_turn(&boot, FIRST_TURN);
    wait_for_turn_completed(&boot).await;
    // Several ticks' worth: an entry still queued would hard-fire on the
    // first of them.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        boot.daemon.turn_start_count_for_test(),
        1,
        "nothing was left to drain"
    );

    let later = queue_one(&boot, "a later one").await;
    wait_until("the second turn/start", || {
        boot.daemon.turn_start_count_for_test() >= 2
    })
    .await;
    let started = boot.daemon.started_turns_for_test();
    assert_eq!(started.len(), 2);
    let second_turn_text = texts_of(&started[1].1).join("\n");
    assert!(
        second_turn_text.contains("a later one"),
        "the later message went out: {second_turn_text}"
    );
    assert!(
        !second_turn_text.contains("steered once"),
        "the steered sentence must not be delivered a second time: {second_turn_text}"
    );
    assert_eq!(
        boot.daemon.started_turn_client_ids_for_test()[1].as_deref(),
        Some(later.as_str())
    );
}

/// An attachment rides as one `localImage` after the text, from the path the
/// bind recorded — the drain's shape for a single entry.
#[tokio::test]
async fn a_steered_entry_carries_its_attachments_as_local_images() {
    use crate::support::planner_queue_fixture::{post_input_with_attachments, upload_png};

    let boot = boot_with_a_running_turn().await;
    let (status, uploaded) = upload_png(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        b"steer-attachment-bytes",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={uploaded}");
    let attachment_id = uploaded["attachmentId"].as_str().unwrap().to_string();
    let (status, posted) = post_input_with_attachments(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "look at this now",
        std::slice::from_ref(&attachment_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={posted}");
    let entry_id = posted["entry_id"].as_str().unwrap().to_string();

    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let steered = boot.daemon.steered_turns_for_test();
    assert_eq!(steered.len(), 1);
    let items = &steered[0].2;
    assert_eq!(items.len(), 2, "text then one image: {items:?}");
    assert_eq!(
        serde_json::to_value(&items[0]).unwrap(),
        json!({ "type": "text", "text": "User says:\nlook at this now" })
    );
    let InputItem::LocalImage { path } = &items[1] else {
        panic!("second item is the image: {items:?}");
    };
    assert!(
        path.starts_with(boot.bound_dir().to_str().unwrap()),
        "the bound path, verified at bind time: {path}"
    );
    // And the row carries it too, for the transcript — the same bound file
    // the image item names.
    let rows = user_rows(&transcript(&boot).await);
    let projection = rows
        .iter()
        .find(|row| row["item_uuid"] == entry_id)
        .expect("the steer's row");
    let attachments = projection["input_segments"][0]["attachments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(attachments.len(), 1, "{projection:?}");
    let bound_id = attachments[0]["id"]
        .as_str()
        .expect("a bound attachment id");
    assert!(
        path.ends_with(&format!("/{bound_id}")),
        "the id carries its extension: {path} vs {bound_id}"
    );
}

// ---------------------------------------------------------------------------
// The refusal matrix — in every arm the entry stays queued, id and rev intact
// ---------------------------------------------------------------------------

/// No running turn: nothing is taken, codex is not asked, nothing is
/// announced, and the entry is exactly where it was.
#[tokio::test]
async fn a_steer_with_no_running_turn_is_a_typed_409_and_changes_nothing() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let entry_id = queue_one(&boot, "waiting").await;

    let cases = [
        (None, "idle"),
        (
            Some(HarnessState::Issuing {
                since: Instant::now(),
                kind: IssuingKind::TurnStart,
            }),
            "issuing_turn",
        ),
        (
            Some(HarnessState::Issuing {
                since: Instant::now(),
                kind: IssuingKind::Interrupt {
                    target_turn_id: "t".into(),
                    reason: "stop".into(),
                },
            }),
            "issuing_interrupt",
        ),
        (
            Some(HarnessState::TurnCompleted {
                last_turn_id: "t".into(),
            }),
            "turn_completed",
        ),
        (
            Some(HarnessState::Wedged {
                since: Instant::now(),
                reason: "interrupt_timeout".into(),
            }),
            "wedged",
        ),
        (
            Some(HarnessState::PendingThreadStart),
            "pending_thread_start",
        ),
    ];
    for (state, phase) in cases {
        if let Some(state) = state {
            boot.harness.set_state_for_test(state).await;
        }
        let (status, body) = steer(&boot, &entry_id, 0).await;
        assert_eq!(status, StatusCode::CONFLICT, "phase={phase} body={body}");
        assert_eq!(
            body["code"],
            json!("planner_steer_no_running_turn"),
            "{body}"
        );
        assert_eq!(body["entry_id"], json!(entry_id));
        assert_eq!(body["phase"], json!(phase), "{body}");
        let message = body["error"].as_str().unwrap_or_default();
        assert!(
            message.contains("still queued") && message.contains("next turn"),
            "the sentence says what happens next: {message}"
        );

        let listed = pending(&boot).await;
        assert_eq!(listed.len(), 1, "phase={phase}");
        assert_eq!(listed[0]["entry_id"], json!(entry_id));
        assert_eq!(
            listed[0]["rev"],
            json!(0),
            "the rev the client read is still valid"
        );
    }
    assert!(
        boot.daemon.steered_turns_for_test().is_empty(),
        "codex was never asked"
    );
    assert!(
        boot.event_payloads("harness.queue.changed")
            .await
            .is_empty(),
        "nothing left the queue, so nothing is announced"
    );
    assert!(
        user_rows(&transcript(&boot).await).is_empty(),
        "no row was written for an undelivered sentence"
    );
}

/// The queue's own refusals keep their shapes, and come before the phase:
/// a stale rev is `planner_input_stale` even with a turn running, an unknown
/// id is 404 whatever the phase, and an agent is refused at the door.
#[tokio::test]
async fn stale_rev_unknown_id_and_agents_are_refused_the_way_the_other_verbs_refuse_them() {
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, "mine").await;

    let (status, body) = steer(&boot, &entry_id, 5).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("planner_input_stale"));
    assert_eq!(body["text"], json!("mine"));
    assert_eq!(body["rev"], json!(0));

    let (status, body) = steer(&boot, "no-such-entry", 0).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");

    for agent in ["ai:claude", "ai:codex", "ai:planner"] {
        let (status, body) = send_json(
            boot.app.clone(),
            "POST",
            steer_uri(boot.planner_card.id.as_str(), &entry_id),
            agent,
            json!({"if_entry_rev": 0}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "actor={agent} body={body}");
    }

    assert_eq!(pending(&boot).await.len(), 1, "nothing was taken");
    assert!(boot.daemon.steered_turns_for_test().is_empty());

    // The paired green: the same request with the rev the client holds.
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
}

/// Codex says no: the entry is taken, asked about, refused, and put back at
/// the head with its id and rev; no row was ever written for it (review
/// round 1: the row follows codex's yes), and its return is announced so a
/// client that read the queue during the RPC learns the entry is back.
#[tokio::test]
async fn a_codex_refusal_puts_the_entry_back_at_the_head_and_announces_the_return() {
    let boot = boot_with_a_running_turn().await;
    let second = queue_one(&boot, "second").await;
    let third = queue_one(&boot, "third").await;
    boot.daemon.reject_turn_steer_for_test(Some(NO_ACTIVE_TURN));

    let (status, body) = steer(&boot, &third, 0).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("planner_steer_no_running_turn"));
    assert_eq!(body["entry_id"], json!(third));
    assert_eq!(body["phase"], json!("turn_running"));
    let message = body["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("no active turn to steer") && message.contains("next turn"),
        "codex's sentence and what happens next: {message}"
    );

    // Codex WAS asked this time.
    let steered = boot.daemon.steered_turns_for_test();
    assert_eq!(steered.len(), 1);
    assert_eq!(steered[0].3.as_deref(), Some(third.as_str()));

    // Back at the head, same id, same rev — the rev does NOT move here
    // (review round 2): this client was told no in the same round trip and
    // hides nothing, and a bump would turn its retry at 0 into a false
    // `stale`. The completion sweep's restore is the one that bumps; see
    // `a_restored_entry_lists_one_rev_up_so_the_client_that_saw_it_leave_can_tell`.
    let listed = pending(&boot).await;
    assert_eq!(
        listed
            .iter()
            .map(|entry| entry["entry_id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![third.clone(), second],
        "the refused entry is re-buffered at the head: {listed:?}"
    );
    assert_eq!(listed[0]["rev"], json!(0));
    assert_eq!(listed[0]["text"], json!("third"));

    // No row was written, so none is announced.
    let rows = user_rows(&transcript(&boot).await);
    assert!(
        rows.iter().all(|row| row["item_uuid"] != third),
        "a refused steer leaves no transcript row: {rows:?}"
    );
    assert!(
        boot.event_payloads("harness.item.added")
            .await
            .iter()
            .all(|payload| payload["item_uuid"] != third),
        "and none was announced"
    );
    // The return IS announced: the entry was out of the queue for the length
    // of the RPC, and any client that read `/planner/run` inside that window
    // is told to read again. Both keys of the plan ride on this one event
    // (`fe/core/events/invalidation-plan.ts`: planner-run + harness-items).
    let changes = boot.event_payloads("harness.queue.changed").await;
    assert_eq!(
        changes,
        vec![json!({
            "worker_session_id": boot.worker_session_id,
            "card_id": boot.planner_card.id.as_str(),
            "track_id": boot.planner_card.track_id.as_str(),
            "entry_id": third,
            "change": "restored",
            "actor": {"kind": "User"},
        })]
    );

    // The paired green: codex accepting again takes the same entry at the
    // same rev.
    boot.daemon.reject_turn_steer_for_test(None);
    let (status, body) = steer(&boot, &third, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        queue_changes(&boot).await,
        vec![
            (third.clone(), "restored".into()),
            (third, "steered".into())
        ]
    );
}

/// Codex says nothing: the request times out. The entry goes back exactly as
/// on a refusal, but the 409 carries its own code, because on this side it is
/// NOT known whether the turn took the message (review round 1, finding 4).
#[tokio::test]
async fn a_steer_codex_never_answered_is_a_typed_unknown_outcome_and_the_entry_is_back() {
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, "did it land?").await;
    boot.daemon.fail_turn_steer_for_test(true);

    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_steer_unknown_outcome"),
        "not `planner_steer_no_running_turn`: that code claims nothing happened, and \
         this side cannot claim it — {body}"
    );
    assert_eq!(body["entry_id"], json!(entry_id));
    assert_eq!(body["phase"], json!("turn_running"));
    let message = body["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("not known")
            && message.contains("timed out")
            && message.contains("next turn"),
        "the sentence says the outcome is unknown and what happens next: {message}"
    );

    // Codex WAS asked (the frame went out; only the answer is missing).
    assert_eq!(boot.daemon.steered_turns_for_test().len(), 1);
    let listed = pending(&boot).await;
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["entry_id"], json!(entry_id));
    assert_eq!(listed[0]["rev"], json!(0));
    assert!(
        user_rows(&transcript(&boot).await)
            .iter()
            .all(|row| row["item_uuid"] != entry_id),
        "no row: codex has not said yes"
    );
    assert_eq!(
        queue_changes(&boot).await,
        vec![(entry_id.clone(), "restored".into())]
    );

    // The paired green, and the paired code: codex answering no is the other
    // code, on the same entry at the same rev.
    boot.daemon.fail_turn_steer_for_test(false);
    boot.daemon.reject_turn_steer_for_test(Some(NO_ACTIVE_TURN));
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("planner_steer_no_running_turn"));
    boot.daemon.reject_turn_steer_for_test(None);
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
}

/// Review round 1, 3b — the documented order: the queue's refusals are
/// answered before the phase is looked at. An unknown id while nothing is
/// running is 404, not the steer's 409; a stale rev in the same phase is
/// `planner_input_stale`, not `planner_steer_no_running_turn`.
#[tokio::test]
async fn queue_refusals_win_over_the_phase_gate_when_no_turn_is_running() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let entry_id = queue_one(&boot, "idle").await;
    assert!(matches!(
        boot.harness.state_for_test().await,
        HarnessState::Idle
    ));

    let (status, body) = steer(&boot, "no-such-entry", 0).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the entry the person pointed at is not there, whatever the phase: {body}"
    );

    let (status, body) = steer(&boot, &entry_id, 7).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_input_stale"),
        "the rev they read is stale, whatever the phase: {body}"
    );
    assert_eq!(body["rev"], json!(0));

    // The paired case: the entry is there at the rev they read, and THEN the
    // phase answers.
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("planner_steer_no_running_turn"));
    assert_eq!(body["phase"], json!("idle"));
}

/// Review round 1, 3a — §4.5's departure rule on the steer: taking the only
/// hard-fire entry out must disarm a queue that still holds a soft
/// observation, or the turn's completion would be followed by a turn nobody
/// asked for, carrying the observation alone.
#[tokio::test]
async fn steering_the_only_user_entry_out_disarms_a_queue_of_observations() {
    let boot = boot_with_a_running_turn().await;
    boot.harness
        .observe_for_test(
            Observation::TrackGoal {
                text: "a soft observation".into(),
            },
            None,
        )
        .await;
    let entry_id = queue_one(&boot, "the hard one").await;
    assert!(
        boot.harness.debounce_hard_fire_for_test().await,
        "premise: the send armed the queue"
    );
    assert_eq!(
        pending(&boot).await.len(),
        1,
        "the strip lists user entries only"
    );

    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    assert!(
        !boot.harness.debounce_hard_fire_for_test().await,
        "nothing hard-fire is left, so the arming must be gone"
    );
    assert_eq!(
        boot.harness.debounce_timestamps_set_for_test().await,
        (true, true),
        "the queue is NOT empty, so the surviving observation keeps the window it was \
         enqueued with"
    );
    assert_eq!(boot.harness.pending_len_for_test().await, 1);

    // The consequence the rule exists for: after codex records the entry
    // and the turn ends, several ticks pass and no turn is issued for the
    // observation alone.
    echo_steered(&boot, &entry_id, "item-user-codex-4", "the hard one");
    wait_for_upgrade(&boot, "item-user-codex-4").await;
    complete_turn(&boot, FIRST_TURN);
    wait_for_turn_completed(&boot).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        boot.daemon.turn_start_count_for_test(),
        1,
        "a soft observation does not fire a turn on its own"
    );
}

// ---------------------------------------------------------------------------
// Review round 1 — a steer codex accepted, then dropped at the interrupt
// ---------------------------------------------------------------------------

/// After `restore_steered_entries_codex_dropped` ran for `entry_id`: the
/// return is announced, the entry goes out once with the next turn under its
/// own id, and one row stands for it at the end — the drain's, not the
/// steer's.
async fn assert_restored_and_delivered_once_by_the_next_turn(
    boot: &Boot,
    entry_id: &str,
    text: &str,
    steer_row_id: i64,
) {
    wait_until("the restored entry to drain into a second turn", || {
        boot.daemon.turn_start_count_for_test() >= 2
    })
    .await;
    let started = boot.daemon.started_turns_for_test();
    assert_eq!(started.len(), 2, "{started:?}");
    let deliveries = started
        .iter()
        .flat_map(|(_, items)| texts_of(items))
        .map(|turn_text| turn_text.matches(text).count())
        .sum::<usize>();
    assert_eq!(
        deliveries, 1,
        "delivered exactly once across every turn/start: {started:?}"
    );
    assert_eq!(
        boot.daemon.started_turn_client_ids_for_test()[1].as_deref(),
        Some(entry_id),
        "the same entry, same id, keyed the next turn"
    );
    assert_eq!(boot.daemon.steered_turns_for_test().len(), 1);
    assert!(pending(boot).await.is_empty());
    assert_eq!(
        queue_changes(boot).await,
        vec![
            (entry_id.to_string(), "steered".into()),
            (entry_id.to_string(), "restored".into()),
        ],
        "the audit log says it left and came back"
    );
    let restored = boot
        .event_payloads("harness.queue.changed")
        .await
        .into_iter()
        .find(|payload| payload["change"] == "restored")
        .unwrap();
    assert_eq!(
        restored["actor"],
        json!({"kind": "Kernel"}),
        "the completion sweep is the kernel's own doing"
    );
    let rows = user_rows(&transcript(boot).await);
    let keyed = rows
        .iter()
        .filter(|row| row["item_uuid"] == entry_id)
        .collect::<Vec<_>>();
    assert_eq!(
        keyed.len(),
        1,
        "one row for the sentence — the drain's, keyed by the same id: {rows:?}"
    );
    assert_ne!(
        keyed[0]["id"].as_i64().unwrap(),
        steer_row_id,
        "the steer's row was deleted, not reused: {rows:?}"
    );
}

async fn steer_row_id(boot: &Boot, entry_id: &str) -> i64 {
    user_rows(&transcript(boot).await)
        .iter()
        .find(|row| row["item_uuid"] == entry_id)
        .expect("the steer's projection row")["id"]
        .as_i64()
        .unwrap()
}

/// The MAJOR of review round 1. Codex accepted the steer, then Stop was
/// pressed before its next model request: codex clears the turn's pending
/// input on the interrupt, the turn completes `interrupted` with no echo for
/// the entry, and — before this round — the queue was empty, the row said
/// the sentence was sent, and it had reached nobody. Now the `TurnCompleted`
/// arm's interrupt-target branch finds the row still a projection, deletes
/// it, and puts the entry back so the next turn carries it.
#[tokio::test]
async fn an_accepted_steer_the_stop_dropped_is_restored_and_goes_out_with_the_next_turn() {
    const TEXT: &str = "lost at the stop, once";
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, TEXT).await;
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let steer_row = steer_row_id(&boot, &entry_id).await;

    // The production Stop path, then codex's answer to it: `turn/completed`
    // with `status: interrupted` and NO echo for the steered entry.
    boot.harness.interrupt("user".into()).await.unwrap();
    assert_eq!(
        boot.daemon.interrupted_turns_for_test(),
        vec![(SEED_THREAD_ID.to_string(), FIRST_TURN.to_string())]
    );
    end_turn(&boot, FIRST_TURN, "interrupted");
    // No wait on `TurnCompleted` here: the restored entry hard-fires, so
    // the phase moves on to the next turn within a tick of the completion.

    assert_restored_and_delivered_once_by_the_next_turn(&boot, &entry_id, TEXT, steer_row).await;
}

/// The same drop through the OTHER branch of the arm — a completion with no
/// interrupt pending (a model error ends the turn before its next request;
/// codex's own watchdog). Both branches call the sweep; a sweep on one
/// branch only would pass the test above and fail this one.
#[tokio::test]
async fn an_accepted_steer_the_turn_failed_under_is_restored_too() {
    const TEXT: &str = "lost to a failed turn, once";
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, TEXT).await;
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let steer_row = steer_row_id(&boot, &entry_id).await;

    end_turn(&boot, FIRST_TURN, "failed");

    assert_restored_and_delivered_once_by_the_next_turn(&boot, &entry_id, TEXT, steer_row).await;
}

/// Review round 2 (F1) — the restored entry comes back ONE REV UP, and that
/// is a client-visible fact, not bookkeeping. The client whose steer
/// answered 200 hides the entry until the server's page stops listing it;
/// after this restore the page lists the same id again, and the only thing
/// on that page that can say "the kernel put it back" rather than "your
/// page is stale" is the rev. So: `GET /planner/run` lists it at rev 1, a
/// steer at the rev the client read (0) is `planner_input_stale` naming 1,
/// and the delete at 1 is the paired green.
///
/// Codex is made to refuse `turn/start` before the turn ends, so the
/// restored entry — which hard-fires — is re-buffered by the drain (which
/// keeps the rev) and paced, instead of leaving the queue within a tick of
/// coming back.
#[tokio::test]
async fn a_restored_entry_lists_one_rev_up_so_the_client_that_saw_it_leave_can_tell() {
    const TEXT: &str = "back, and visibly so";
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, TEXT).await;
    assert_eq!(pending(&boot).await[0]["rev"], json!(0), "premise");
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert!(pending(&boot).await.is_empty(), "out of the queue");

    boot.daemon.reject_turn_start_for_test();
    boot.harness.interrupt("user".into()).await.unwrap();
    end_turn(&boot, FIRST_TURN, "interrupted");
    wait_until(
        "the restored entry to be re-buffered by a refused drain",
        || boot.harness.refused_issuances_for_test() >= 1,
    )
    .await;

    let listed = pending(&boot).await;
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["entry_id"], json!(entry_id));
    assert_eq!(listed[0]["text"], json!(TEXT), "the same sentence");
    assert_eq!(
        listed[0]["rev"],
        json!(1),
        "one rev up: the page that lists it again is not the page from before the steer"
    );
    assert_eq!(
        queue_changes(&boot).await,
        vec![
            (entry_id.clone(), "steered".into()),
            (entry_id.clone(), "restored".into()),
        ]
    );

    // The CAS token really moved: the rev the client read is stale now, and
    // the refusal names the rev to re-read at.
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("planner_input_stale"));
    assert_eq!(body["rev"], json!(1));
    assert_eq!(body["text"], json!(TEXT));

    // The paired green, at the rev the page lists.
    let (status, body) = send_json(
        boot.app.clone(),
        "DELETE",
        format!(
            "/api/cards/{}/planner/input/{entry_id}",
            boot.planner_card.id.as_str()
        ),
        "user",
        json!({"if_entry_rev": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert!(pending(&boot).await.is_empty());
}

/// The inverse, so the sweep cannot be read as "every steered entry comes
/// back at the interrupt": codex echoed the entry (its next model request
/// recorded it) BEFORE Stop was pressed. The echo upgraded the row, so the
/// row is no longer a projection, the sweep leaves it alone, and the entry
/// is NOT re-queued — re-delivering it would say the sentence twice.
#[tokio::test]
async fn an_accepted_steer_codex_recorded_before_the_stop_is_not_restored() {
    const TEXT: &str = "recorded, then stopped";
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, TEXT).await;
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let steer_row = steer_row_id(&boot, &entry_id).await;

    echo_steered(&boot, &entry_id, "item-user-codex-9", TEXT);
    wait_for_upgrade(&boot, "item-user-codex-9").await;

    boot.harness.interrupt("user".into()).await.unwrap();
    end_turn(&boot, FIRST_TURN, "interrupted");
    wait_for_turn_completed(&boot).await;
    // Several ticks' worth: a re-queued entry would hard-fire on the first.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        boot.daemon.turn_start_count_for_test(),
        1,
        "nothing was re-queued, so nothing drained"
    );
    assert!(pending(&boot).await.is_empty());
    assert_eq!(
        queue_changes(&boot).await,
        vec![(entry_id.clone(), "steered".into())],
        "no `restored`: codex has the sentence"
    );
    let rows = user_rows(&transcript(&boot).await);
    let upgraded = rows
        .iter()
        .find(|row| row["item_uuid"] == "item-user-codex-9")
        .expect("the upgraded row still stands");
    assert_eq!(upgraded["id"].as_i64().unwrap(), steer_row, "the same row");
    assert_eq!(upgraded["turn_id"], FIRST_TURN);
    assert!(
        rows.iter().all(|row| row["item_uuid"] != entry_id),
        "and no projection under the entry id is left beside it: {rows:?}"
    );
}

// ---------------------------------------------------------------------------
// The race the on-loop shape exists for
// ---------------------------------------------------------------------------

/// The turn completes while `turn/steer` is in flight. The completion cannot
/// be processed until the steer is answered (same `select!`), so the refused
/// entry is back at the head BEFORE the tick that drains it can run — and it
/// goes out exactly once, in the next turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_completing_during_the_steer_leaves_the_entry_queued_exactly_once() {
    const TEXT: &str = "sent once and only once";
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, TEXT).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    boot.daemon
        .install_turn_steer_return_hook_for_test(TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        });

    let app = boot.app.clone();
    let uri = steer_uri(boot.planner_card.id.as_str(), &entry_id);
    let in_flight = tokio::spawn(async move {
        send_json(app, "POST", uri, "user", json!({"if_entry_rev": 0})).await
    });
    entered.notified().await;

    // The entry has provably left the queue and reached the daemon; now the
    // turn ends under it. The completion sits in the notification channel:
    // the loop is inside the steer and cannot take it yet.
    assert!(
        boot.harness.pending_entries_for_test().await.is_empty(),
        "taken out before codex was asked"
    );
    // … and the snapshot ON DISK still lists it: the removal is persisted
    // only once codex has said yes, so a crash inside this window re-drains
    // the entry rather than losing it (the declared crash window).
    assert_eq!(
        persisted_pending_ids(&boot).await,
        vec![entry_id.clone()],
        "the pre-answer snapshot still lists the entry"
    );
    complete_turn(&boot, FIRST_TURN);
    boot.daemon.reject_turn_steer_for_test(Some(NO_ACTIVE_TURN));
    release.notify_one();

    let (status, body) = in_flight.await.expect("the steer task finishes");
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("planner_steer_no_running_turn"));

    // Now the completion lands, the queue holds the entry once, and the tick
    // drains it into the next turn once.
    boot.daemon.reject_turn_steer_for_test(None);
    wait_until("the re-buffered entry to drain into a second turn", || {
        boot.daemon.turn_start_count_for_test() >= 2
    })
    .await;
    let started = boot.daemon.started_turns_for_test();
    assert_eq!(started.len(), 2, "{started:?}");
    let deliveries = started
        .iter()
        .flat_map(|(_, items)| texts_of(items))
        .map(|text| text.matches(TEXT).count())
        .sum::<usize>();
    assert_eq!(
        deliveries, 1,
        "delivered exactly once across every turn/start: {started:?}"
    );
    assert_eq!(boot.daemon.steered_turns_for_test().len(), 1);
    assert!(pending(&boot).await.is_empty());
    let rows = user_rows(&transcript(&boot).await);
    assert_eq!(
        rows.iter()
            .filter(|row| row["item_uuid"] == entry_id)
            .count(),
        1,
        "one row for the sentence — the drain's, keyed by the same id: {rows:?}"
    );
}

#[tokio::test]
async fn a_system_error_retains_a_dropped_steer_in_the_failed_snapshot() {
    const TEXT: &str = "retain this steer across recovery";
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, TEXT).await;
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    boot.daemon
        .emit_notification_for_test(Notification::ThreadStatusChanged {
            thread_id: SEED_THREAD_ID.into(),
            status: json!({"type":"systemError"}),
        });
    end_turn(&boot, FIRST_TURN, "failed");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let row = boot
            .repo
            .session_projection_by_id(&boot.worker_session_id)
            .await
            .unwrap()
            .unwrap();
        let snapshot = HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap());
        if row.status == calm_server::session_projection_repo::WorkerSessionState::Failed
            && snapshot
                .pending_entries()
                .iter()
                .any(|e| e.id().is_some_and(|id| id.as_str() == entry_id))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "dropped steer must be durable in failed snapshot"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        boot.daemon.turn_start_count_for_test(),
        1,
        "no automatic quota retry"
    );
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn immediate_recovery_waits_for_the_failed_turn_to_settle_accepted_steers() {
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, "not yet reconciled").await;
    assert_eq!(steer(&boot, &entry_id, 0).await.0, StatusCode::OK);
    boot.daemon
        .emit_notification_for_test(Notification::ThreadStatusChanged {
            thread_id: SEED_THREAD_ID.into(),
            status: json!({"type":"systemError"}),
        });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if boot
            .repo
            .session_projection_by_id(&boot.worker_session_id)
            .await
            .unwrap()
            .unwrap()
            .status
            == calm_server::session_projection_repo::WorkerSessionState::Failed
        {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (status, body) = post_input(
        boot.app.clone(),
        boot.planner_card.id.as_str(),
        "resume now",
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("settle its messages")
    );
    assert!(boot.daemon.resumed_threads_for_test().is_empty());
    end_turn(&boot, FIRST_TURN, "failed");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let row = boot
            .repo
            .session_projection_by_id(&boot.worker_session_id)
            .await
            .unwrap()
            .unwrap();
        let snapshot = HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap());
        if snapshot
            .pending_entries()
            .iter()
            .any(|e| e.id().is_some_and(|id| id.as_str() == entry_id))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "refused immediate recovery must keep the settling loop alive"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    boot.harness.shutdown().await.unwrap();
}
