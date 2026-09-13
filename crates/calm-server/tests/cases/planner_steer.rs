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
use calm_server::harness::{HarnessState, IssuingKind};
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
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: SEED_THREAD_ID.to_string(),
            turn: json!({ "id": turn_id, "status": "completed" }),
        });
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

    // The queue no longer lists it.
    assert!(pending(&boot).await.is_empty());

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

/// The double-delivery test. Once codex has the entry, the queue must not:
/// after the running turn completes, the tick finds nothing to drain, and a
/// later message starts a turn that carries only itself.
#[tokio::test]
async fn a_steered_entry_is_not_drained_again_when_the_turn_completes() {
    let boot = boot_with_a_running_turn().await;
    let entry_id = queue_one(&boot, "steered once").await;
    let (status, body) = steer(&boot, &entry_id, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");

    complete_turn(&boot, FIRST_TURN);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if matches!(
            boot.harness.state_for_test().await,
            HarnessState::TurnCompleted { .. }
        ) {
            break;
        }
        assert!(Instant::now() < deadline, "the completion never landed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
/// the head with its id and rev; the row that said it was sent is gone and
/// was never announced.
#[tokio::test]
async fn a_codex_refusal_puts_the_entry_back_at_the_head_and_deletes_its_row() {
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

    // Back at the head, same id, same rev.
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

    // No row, no announcement.
    let rows = user_rows(&transcript(&boot).await);
    assert!(
        rows.iter().all(|row| row["item_uuid"] != third),
        "the projection of a refused steer is deleted: {rows:?}"
    );
    assert!(
        boot.event_payloads("harness.item.added")
            .await
            .iter()
            .all(|payload| payload["item_uuid"] != third),
        "and it was never announced"
    );
    assert!(
        boot.event_payloads("harness.queue.changed")
            .await
            .is_empty()
    );

    // The paired green: codex accepting again takes the same entry at the
    // same rev.
    boot.daemon.reject_turn_steer_for_test(None);
    let (status, body) = steer(&boot, &third, 0).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
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
