//! Live reply text (#1923 S2) through the production run loop and `GET /api/cards/{id}/harness/live`:
//! Codex notifications go in through the daemon's fan-out, and the answer comes back from the route the
//! harness's registry serves.

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use calm_server::codex_appserver::Notification;
use calm_server::harness::{HarnessState, IssuingKind};
use serde_json::{Value, json};

use crate::support::planner_queue_fixture::{Boot, SEED_THREAD_ID, boot_with, get, idle_snapshot};

const TURN: &str = "turn-live-1";
/// The transcript's table, renamed away to make an item row fail to store.
const ITEM_TABLE: &str = "harness_items";

async fn boot() -> Boot {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while boot.daemon.notification_receiver_count_for_test() == 0 {
        assert!(Instant::now() < deadline, "the harness never subscribed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    boot
}

fn item(boot: &Boot, method: &str, params: Value) {
    boot.daemon.emit_notification_for_test(Notification::Item {
        method: method.into(),
        params,
    });
}

fn turn_started(boot: &Boot) {
    boot.daemon.emit_turn_started_for_test(SEED_THREAD_ID, TURN);
}

fn reply_started(boot: &Boot, turn_id: &str, item_id: &str) {
    item(
        boot,
        "item/started",
        json!({
            "threadId": SEED_THREAD_ID,
            "turnId": turn_id,
            "item": { "id": item_id, "type": "agentMessage", "text": "" },
        }),
    );
}

fn delta(boot: &Boot, turn_id: &str, item_id: &str, text: &str) {
    item(
        boot,
        "item/agentMessage/delta",
        json!({ "threadId": SEED_THREAD_ID, "turnId": turn_id, "itemId": item_id, "delta": text }),
    );
}

fn reply_completed(boot: &Boot, item_id: &str, text: &str) {
    item(
        boot,
        "item/completed",
        json!({
            "threadId": SEED_THREAD_ID,
            "turnId": TURN,
            "item": { "id": item_id, "type": "agentMessage", "text": text },
        }),
    );
}

/// A stored item that is not a reply, so its row tells the test the run loop got this far.
fn command_started(boot: &Boot, item_id: &str) {
    item(
        boot,
        "item/started",
        json!({
            "threadId": SEED_THREAD_ID,
            "turnId": TURN,
            "item": { "id": item_id, "type": "commandExecution", "command": "true" },
        }),
    );
}

fn turn_completed(boot: &Boot, status: &str) {
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: SEED_THREAD_ID.into(),
            turn: json!({ "id": TURN, "status": status }),
        });
}

async fn live(boot: &Boot) -> Value {
    let uri = format!("/api/cards/{}/harness/live", boot.planner_card.id.as_str());
    let (status, body) = get(boot.app.clone(), uri).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    body
}

fn live_turn(items: &[(&str, &str)]) -> Value {
    let items: Vec<Value> = items
        .iter()
        .map(|(item_id, text)| json!({ "item_id": item_id, "text": text }))
        .collect();
    json!({ "turn_id": TURN, "items": items })
}

fn no_live_turn() -> Value {
    json!({ "turn_id": null, "items": [] })
}

async fn wait_live(boot: &Boot, expected: Value) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let body = live(boot).await;
        if body == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "live replies never became {expected}; last {body}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The transcript rows, read through `GET harness/items`.
async fn rows(boot: &Boot) -> Vec<Value> {
    let uri = format!("/api/cards/{}/harness/items", boot.planner_card.id.as_str());
    let (status, body) = get(boot.app.clone(), uri).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    body.as_array().unwrap().clone()
}

async fn wait_rows(boot: &Boot, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rows = rows(boot).await;
        if rows.len() >= count {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "expected {count} rows; got {:?}",
            shape(&rows)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn params(row: &Value) -> Value {
    serde_json::from_str(row["params"].as_str().unwrap()).unwrap()
}

/// Interrupt through the harness's own entry point, then answer it as Codex does.
async fn interrupt(boot: &Boot) {
    boot.harness.interrupt("test stop".into()).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(
        boot.harness.state_for_test().await,
        HarnessState::Issuing {
            kind: IssuingKind::Interrupt { .. },
            ..
        }
    ) {
        assert!(Instant::now() < deadline, "the interrupt was never issued");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    turn_completed(boot, "interrupted");
}

/// The row methods in id order, with a partial reply marked.
fn shape(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            let method = row["method"].as_str().unwrap();
            match params(row).get("_partial") {
                Some(Value::Bool(true)) => format!("{method} (partial)"),
                _ => method.to_owned(),
            }
        })
        .collect()
}

#[tokio::test]
async fn a_streamed_reply_is_live_until_its_completion_is_stored() {
    let boot = boot().await;
    wait_live(&boot, no_live_turn()).await;
    turn_started(&boot);
    reply_started(&boot, TURN, "reply-1");
    delta(&boot, TURN, "reply-1", "Hel");
    delta(&boot, TURN, "reply-1", "lo");
    wait_live(&boot, live_turn(&[("reply-1", "Hello")])).await;

    reply_completed(&boot, "reply-1", "Hello");
    let rows = wait_rows(&boot, 2).await;
    assert_eq!(shape(&rows), ["item/started", "item/completed"]);
    assert_eq!(params(&rows[1])["item"]["text"], "Hello");
    wait_live(&boot, live_turn(&[])).await;
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_delta_without_an_observed_start_or_for_another_turn_is_ignored() {
    let boot = boot().await;
    turn_started(&boot);
    delta(&boot, TURN, "never-started", "lost");
    command_started(&boot, "command-1");
    delta(&boot, TURN, "command-1", "not a reply");
    reply_started(&boot, "turn-other", "reply-other");
    delta(&boot, "turn-other", "reply-other", "other turn");
    reply_started(&boot, TURN, "reply-1");
    delta(&boot, "turn-other", "reply-1", "wrong turn");
    delta(&boot, TURN, "reply-1", "kept");
    wait_live(&boot, live_turn(&[("reply-1", "kept")])).await;
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_interrupted_reply_is_stored_once_as_partial_before_the_outcome() {
    let boot = boot().await;
    turn_started(&boot);
    reply_started(&boot, TURN, "reply-1");
    delta(&boot, TURN, "reply-1", "Half a ");
    delta(&boot, TURN, "reply-1", "reply");
    wait_live(&boot, live_turn(&[("reply-1", "Half a reply")])).await;

    interrupt(&boot).await;
    let rows = wait_rows(&boot, 3).await;
    assert_eq!(
        shape(&rows),
        ["item/started", "item/completed (partial)", "turn/completed"]
    );
    let partial = &rows[1];
    assert!(
        partial["id"].as_i64() < rows[2]["id"].as_i64(),
        "the partial precedes the outcome"
    );
    assert_eq!(partial["turn_id"], TURN);
    assert_eq!(partial["item_uuid"], "reply-1");
    assert_eq!(partial["item_type"], "agentMessage");
    let stored = params(partial);
    assert_eq!(
        stored["item"],
        json!({ "id": "reply-1", "type": "agentMessage", "text": "Half a reply" })
    );
    assert_eq!(stored["threadId"], SEED_THREAD_ID);
    assert_eq!(stored["turnId"], TURN);
    let announced: Vec<Value> = boot
        .event_payloads("harness.item.added")
        .await
        .into_iter()
        .filter(|event| event["item_db_id"] == partial["id"])
        .collect();
    assert_eq!(
        announced.len(),
        1,
        "the partial row is announced like any item"
    );
    assert_eq!(announced[0]["method"], "item/completed");
    wait_live(&boot, no_live_turn()).await;

    // A repeated completion stores no second partial.
    turn_completed(&boot, "interrupted");
    command_started(&boot, "barrier");
    let rows = wait_rows(&boot, 4).await;
    assert_eq!(rows.len(), 4, "{:?}", shape(&rows));
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failed_turn_after_a_system_error_stores_its_partial_reply() {
    let boot = boot().await;
    turn_started(&boot);
    reply_started(&boot, TURN, "reply-1");
    delta(&boot, TURN, "reply-1", "cut short");
    wait_live(&boot, live_turn(&[("reply-1", "cut short")])).await;

    boot.daemon
        .emit_notification_for_test(Notification::ThreadStatusChanged {
            thread_id: SEED_THREAD_ID.into(),
            status: json!({ "type": "systemError" }),
        });
    turn_completed(&boot, "failed");
    let rows = wait_rows(&boot, 3).await;
    assert_eq!(
        shape(&rows),
        ["item/started", "item/completed (partial)", "turn/completed"]
    );
    assert_eq!(params(&rows[1])["item"]["text"], "cut short");
    wait_live(&boot, no_live_turn()).await;
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_completed_turn_discards_a_reply_it_abandoned() {
    let boot = boot().await;
    turn_started(&boot);
    reply_started(&boot, TURN, "abandoned");
    delta(&boot, TURN, "abandoned", "retried away");
    wait_live(&boot, live_turn(&[("abandoned", "retried away")])).await;

    turn_completed(&boot, "completed");
    let rows = wait_rows(&boot, 2).await;
    wait_live(&boot, no_live_turn()).await;
    command_started(&boot, "barrier");
    let rows_after = wait_rows(&boot, 3).await;
    assert_eq!(shape(&rows), ["item/started", "turn/completed"]);
    assert_eq!(rows_after.len(), 3, "{:?}", shape(&rows_after));
    boot.harness.shutdown().await.unwrap();
}

/// The fake daemon's fan-out holds 16 frames; the test's synchronous burst overflows it before the
/// run loop runs again, so the run loop's next receive is `Lagged`.
#[tokio::test]
async fn lagged_input_blocks_the_turn_and_no_partial_is_stored_even_when_interrupted() {
    let boot = boot().await;
    turn_started(&boot);
    reply_started(&boot, TURN, "reply-1");
    delta(&boot, TURN, "reply-1", "Hel");
    wait_live(&boot, live_turn(&[("reply-1", "Hel")])).await;

    for _ in 0..40 {
        delta(&boot, TURN, "reply-1", "x");
    }
    command_started(&boot, "barrier");
    wait_rows(&boot, 2).await;
    assert_eq!(live(&boot).await, live_turn(&[]));
    reply_started(&boot, TURN, "reply-2");
    delta(&boot, TURN, "reply-2", "after the loss");
    command_started(&boot, "barrier-2");
    wait_rows(&boot, 3).await;
    assert_eq!(live(&boot).await, live_turn(&[]));

    interrupt(&boot).await;
    let rows = wait_rows(&boot, 5).await;
    wait_live(&boot, no_live_turn()).await;
    assert_eq!(
        shape(&rows),
        [
            "item/started",
            "item/started",
            "item/started",
            "item/started",
            "turn/completed"
        ]
    );
    boot.harness.shutdown().await.unwrap();
}

/// Every snapshot write updates `worker_sessions.handle_state_json`; a trigger counts them.
#[tokio::test]
async fn a_delta_writes_no_row_no_event_and_no_snapshot() {
    let boot = boot().await;
    for statement in [
        "CREATE TABLE test_snapshot_writes (n INTEGER NOT NULL)",
        "INSERT INTO test_snapshot_writes (n) VALUES (0)",
        "CREATE TRIGGER test_count_snapshot_writes AFTER UPDATE OF handle_state_json \
         ON worker_sessions BEGIN UPDATE test_snapshot_writes SET n = n + 1; END",
    ] {
        sqlx::query(statement)
            .execute(boot.repo.pool())
            .await
            .unwrap();
    }
    let writes = || async {
        sqlx::query_scalar::<_, i64>("SELECT n FROM test_snapshot_writes")
            .fetch_one(boot.repo.pool())
            .await
            .unwrap()
    };
    let events = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM events")
            .fetch_one(boot.repo.pool())
            .await
            .unwrap()
    };
    turn_started(&boot);
    reply_started(&boot, TURN, "reply-1");
    wait_rows(&boot, 1).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while writes().await < 2 {
        assert!(
            Instant::now() < deadline,
            "the start never wrote its snapshot"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (writes_before, events_before) = (writes().await, events().await);

    let mut text = String::new();
    for batch in 0..3 {
        // Below the fan-out's capacity, so nothing lags.
        for piece in 0..10 {
            let piece = format!("{batch}{piece} ");
            delta(&boot, TURN, "reply-1", &piece);
            text.push_str(&piece);
        }
        wait_live(&boot, live_turn(&[("reply-1", text.as_str())])).await;
    }
    assert_eq!(rows(&boot).await.len(), 1, "a delta stores no row");
    assert_eq!(events().await, events_before, "a delta logs no event");
    assert!(
        writes().await - writes_before <= 1,
        "30 deltas wrote the snapshot {} times",
        writes().await - writes_before
    );
    boot.harness.shutdown().await.unwrap();
}

async fn rename_table(boot: &Boot, from: &str, to: &str) {
    sqlx::query(&format!("ALTER TABLE {from} RENAME TO {to}"))
        .execute(boot.repo.pool())
        .await
        .unwrap();
}

/// The text is always live or durable: a completion whose row did not land leaves the reply open, and
/// an interrupt then stores it.
#[tokio::test]
async fn a_reply_whose_completion_fails_to_store_stays_live() {
    let boot = boot().await;
    turn_started(&boot);
    reply_started(&boot, TURN, "reply-1");
    delta(&boot, TURN, "reply-1", "Hel");
    wait_live(&boot, live_turn(&[("reply-1", "Hel")])).await;

    rename_table(&boot, ITEM_TABLE, "hidden_rows").await;
    reply_completed(&boot, "reply-1", "Hel");
    delta(&boot, TURN, "reply-1", "lo");
    wait_live(&boot, live_turn(&[("reply-1", "Hello")])).await;
    rename_table(&boot, "hidden_rows", ITEM_TABLE).await;

    interrupt(&boot).await;
    let rows = wait_rows(&boot, 3).await;
    assert_eq!(
        shape(&rows),
        ["item/started", "item/completed (partial)", "turn/completed"]
    );
    assert_eq!(params(&rows[1])["item"]["text"], "Hello");
    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_clears_the_cards_live_replies() {
    let boot = boot().await;
    turn_started(&boot);
    reply_started(&boot, TURN, "reply-1");
    delta(&boot, TURN, "reply-1", "mid-reply");
    wait_live(&boot, live_turn(&[("reply-1", "mid-reply")])).await;
    boot.harness.shutdown().await.unwrap();
    wait_live(&boot, no_live_turn()).await;
}
