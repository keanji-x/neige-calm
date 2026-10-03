//! Live reply text (#1923 S2) through the production run loop and `GET /api/cards/{id}/harness/live`:
//! Codex notifications go in through the daemon's fan-out, and the answer comes back from the route the
//! harness's registry serves.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use calm_server::codex_appserver::Notification;
use calm_server::harness::{
    HarnessConfig, HarnessState, IssuingKind, PlannerHarness, PlannerHarnessParams,
};
use calm_server::shared_codex_appserver::TurnStartReturnHook;
use serde_json::{Value, json};

use crate::support::planner_queue_fixture::{
    Boot, Issuance, SEED_THREAD_ID, boot_with_issuance, get, idle_snapshot, post_input,
};

const TURN: &str = "turn-live-1";
/// The transcript's table, renamed away to make an item row fail to store.
pub(crate) const ITEM_TABLE: &str = "harness_items";

async fn boot() -> Boot {
    boot_issuing(Issuance::Paused).await
}

async fn boot_issuing(issuance: Issuance) -> Boot {
    let boot = boot_with_issuance(idle_snapshot(vec![]), issuance).await;
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

/// An `agentMessage` item as Codex 0.159 sends it, with `text`.
fn reply_item(item_id: &str, text: &str) -> Value {
    json!({
        "delivery": null,
        "id": item_id,
        "memoryCitation": null,
        "phase": "final_answer",
        "questions": null,
        "text": text,
        "type": "agentMessage",
    })
}

fn reply_started(boot: &Boot, turn_id: &str, item_id: &str) {
    item(
        boot,
        "item/started",
        json!({
            "threadId": SEED_THREAD_ID,
            "turnId": turn_id,
            "item": reply_item(item_id, ""),
            "startedAtMs": 1,
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
            "item": reply_item(item_id, text),
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
    completed(boot, TURN, status);
}

fn completed(boot: &Boot, turn_id: &str, status: &str) {
    boot.daemon
        .emit_notification_for_test(Notification::TurnCompleted {
            thread_id: SEED_THREAD_ID.into(),
            turn: json!({ "id": turn_id, "status": status }),
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
        reply_item("reply-1", "Half a reply"),
        "the item its start carried, with the streamed text"
    );
    assert_eq!(stored["_partial"], true);
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

/// The phase the run loop last stored for the runtime. Every `TurnCompleted` branch stores it after
/// the settle hook and the outcome row.
async fn wait_stored_phase(boot: &Boot, phase: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT json_extract(handle_state_json, '$.phase') FROM worker_sessions WHERE id = ?",
        )
        .bind(&boot.worker_session_id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
        if stored.as_deref() == Some(phase) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the stored phase never became {phase}; last {stored:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A reply leaves the live state only once its row is stored: a partial that fails to store stays
/// live, so its text is never neither live nor durable.
#[tokio::test]
async fn a_partial_reply_that_fails_to_store_stays_live() {
    let boot = boot().await;
    turn_started(&boot);
    reply_started(&boot, TURN, "reply-1");
    delta(&boot, TURN, "reply-1", "Half");
    wait_live(&boot, live_turn(&[("reply-1", "Half")])).await;

    rename_table(&boot, ITEM_TABLE, "hidden_rows").await;
    interrupt(&boot).await;
    wait_stored_phase(&boot, "turn_completed").await;
    assert_eq!(live(&boot).await, live_turn(&[("reply-1", "Half")]));
    rename_table(&boot, "hidden_rows", ITEM_TABLE).await;
    assert_eq!(shape(&rows(&boot).await), ["item/started"]);
    boot.harness.shutdown().await.unwrap();
}

/// A turn Codex ends as interrupted or failed without an interrupt from neige takes the normal
/// `TurnCompleted` branch, and its partial still precedes the outcome.
#[tokio::test]
async fn a_turn_that_ends_interrupted_or_failed_unasked_stores_its_partial_before_the_outcome() {
    for status in ["interrupted", "failed"] {
        let boot = boot().await;
        turn_started(&boot);
        reply_started(&boot, TURN, "reply-1");
        delta(&boot, TURN, "reply-1", status);
        wait_live(&boot, live_turn(&[("reply-1", status)])).await;
        assert!(matches!(
            boot.harness.state_for_test().await,
            HarnessState::TurnRunning { .. }
        ));

        turn_completed(&boot, status);
        let rows = wait_rows(&boot, 3).await;
        assert_eq!(
            shape(&rows),
            ["item/started", "item/completed (partial)", "turn/completed"],
            "{status}"
        );
        assert!(rows[1]["id"].as_i64() < rows[2]["id"].as_i64());
        assert_eq!(params(&rows[1])["item"], reply_item("reply-1", status));
        wait_live(&boot, no_live_turn()).await;
        boot.harness.shutdown().await.unwrap();
    }
}

/// The interrupt lands while `turn/start` is still in flight, so the run loop drains the turn's
/// `turn/started` in `IssuingInterrupt`, a state that does not accept it. The turn still streams,
/// and its interrupted completion stores the partial.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupt_issued_before_its_turn_start_drains_still_streams_the_turn() {
    let boot = boot_issuing(Issuance::Live).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    boot.daemon
        .install_turn_start_return_hook_for_test(TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        });
    let (status, body) = post_input(boot.app.clone(), boot.planner_card.id.as_str(), "go").await;
    assert!(status.is_success(), "body={body}");
    entered.notified().await;
    let turn = format!("fake-turn-{:04}", boot.daemon.turn_start_count_for_test());
    // Codex knows the turn before its `turn/started` reaches the harness.
    boot.daemon.set_active_turn_for_test(SEED_THREAD_ID, &turn);
    boot.harness.interrupt("test stop".into()).await.unwrap();
    assert!(matches!(
        boot.harness.state_for_test().await,
        HarnessState::Issuing {
            kind: IssuingKind::Interrupt { ref target_turn_id, .. },
            ..
        } if *target_turn_id == turn
    ));
    release.notify_one();
    // `turn/start` returned, so its `turn/started` is queued ahead of the frames below.
    let deadline = Instant::now() + Duration::from_secs(5);
    while boot.harness.snapshot().await.last_turn_id.as_deref() != Some(turn.as_str()) {
        assert!(Instant::now() < deadline, "turn/start never returned");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    reply_started(&boot, &turn, "reply-1");
    delta(&boot, &turn, "reply-1", "cut short");
    let streaming =
        json!({ "turn_id": turn, "items": [{ "item_id": "reply-1", "text": "cut short" }] });
    wait_live(&boot, streaming).await;
    completed(&boot, &turn, "interrupted");
    wait_stored_phase(&boot, "turn_completed").await;
    let rows = rows(&boot).await;
    let partial = rows
        .iter()
        .position(|row| shape(std::slice::from_ref(row)) == ["item/completed (partial)"])
        .unwrap_or_else(|| panic!("no partial row: {:?}", shape(&rows)));
    assert_eq!(
        params(&rows[partial])["item"],
        reply_item("reply-1", "cut short")
    );
    assert_eq!(rows[partial + 1]["method"], "turn/completed");
    wait_live(&boot, no_live_turn()).await;
    boot.harness.shutdown().await.unwrap();
}

/// A concurrent start that loses the registry slot has already built and run its harness. That
/// harness never takes the card's live replies, so shutting it down leaves the installed one
/// streaming.
#[tokio::test]
async fn a_harness_that_loses_its_start_leaves_the_installed_harness_streaming() {
    let boot = boot().await;
    let runtime = "runtime-superseded".to_string();
    let stale = boot.registry.try_reserve(runtime.clone()).unwrap();
    let (_winner, _) = boot.registry.reserve_replacing(runtime.clone());
    let loser = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime,
        track_id: boot.planner_card.track_id.clone(),
        card_id: boot.planner_card.id.clone(),
        thread_id: Some(SEED_THREAD_ID.into()),
        repo: boot.repo.clone(),
        events: calm_server::event::EventBus::new(),
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        track_area_cache: calm_server::track_area_cache::TrackAreaCache::new(),
        backend: boot.daemon.clone().into(),
        live_replies: boot.registry.live_replies().clone(),
        config: HarnessConfig::default(),
        snapshot: idle_snapshot(vec![]),
    });
    assert!(!stale.install(loser.clone()), "the start was superseded");
    loser.shutdown().await.unwrap();

    turn_started(&boot);
    reply_started(&boot, TURN, "reply-1");
    delta(&boot, TURN, "reply-1", "still streaming");
    wait_live(&boot, live_turn(&[("reply-1", "still streaming")])).await;
    boot.harness.shutdown().await.unwrap();
}
