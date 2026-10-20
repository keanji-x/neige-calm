//! #2774 — what a new Planner conversation keeps and refuses beyond its commit: a crash after
//! `TxCommitted` recovers on boot as a failure after the cutover does, never reviving the old
//! session; a forge approval keyed on the card stays Approved; an old turn cannot be replaced; and
//! a Claude Planner's old token stops authenticating.

use axum::http::{Request, StatusCode};
use calm_server::ask::{ForgeApproval, ForgeApprovalRead, forge_approval_tx};
use calm_server::db::prelude::*;
use calm_server::ids::{ActorId, CardId, TrackId};
use calm_server::model::new_id;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use serde_json::json;
use tempfile::TempDir;

use crate::planner_first_start::{AppServer, Boot, app_state_over, post_input, router};
use crate::planner_new_conversation::{START_COMMITTED, pause_at, person, send};
use crate::planner_new_conversation_wakes::finish_turn;
use crate::planner_repoint_restart_lock::{request, wait_until};
use crate::planner_restart::{get, ordinary_track, wait_for};

/// Where a new conversation's install has persisted its catch-up and not yet opened the provider.
const CAUGHT_UP: &str = "conversation-start-caught-up";

async fn state_of(boot: &Boot, session: &str) -> String {
    sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id = ?1")
        .bind(session)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap()
}

/// The new conversation's start, by its stored payload: `(phase, compensation)`.
async fn conversation_start(boot: &Boot) -> (String, Option<String>) {
    sqlx::query_as(
        "SELECT phase, compensation_state FROM operations WHERE kind = 'planner-harness-start' \
           AND json_extract(payload_json, '$.new_conversation') IS NOT NULL",
    )
    .fetch_one(boot.repo.pool())
    .await
    .unwrap()
}

/// Commit the cutover, then lose the process before the new thread is minted: the request never
/// answers and nothing in memory survives. Returns the new session the commit made current and a
/// boot of a fresh process over the same data, before its recovery runs.
async fn crash_after_the_commit(boot: &Boot, key: &str) -> (String, Boot) {
    let (entered, _release) = pause_at(START_COMMITTED, &boot.planner_card_id);
    let pending = {
        let app = person(boot, LEGACY_OWNER_USER_ID);
        let (uri, key) = (boot.card_uri("planner/conversations"), key.to_string());
        tokio::spawn(async move { send(app, "POST", uri, Some(&key), None).await })
    };
    entered.notified().await;
    pending.abort();
    let _ = pending.await;
    wait_until("the lost request released the card", || {
        boot.state
            .planner_recovery_lock_handles_for_test(&boot.planner_card_id)
            == 0
    })
    .await;
    for harness in boot.state.harness.drain_all_for_dev() {
        let _ = harness.shutdown().await;
    }
    // Past `TxCommitted`, entering the provider phase before any provider call.
    assert_eq!(
        conversation_start(boot).await.0,
        "app_server_interact",
        "premise"
    );
    let carrier = boot.current_session().await.unwrap();
    assert_eq!(state_of(boot, &carrier).await, "starting", "premise");
    let state = app_state_over(boot._tmp.path(), &boot.repo, AppServer::Running).await;
    // A fresh fake daemon numbers its threads from one again, and real thread ids never repeat.
    let minted = boot
        .state
        .shared_codex_appserver
        .started_thread_params_for_test()
        .len();
    for _ in 0..minted {
        state
            .shared_codex_appserver
            .thread_start_for_card(
                "an earlier process",
                calm_server::model::CardRole::Worker,
                None,
                calm_server::shared_codex_appserver::SharedThreadStartParams {
                    cwd: "/".into(),
                    approval_policy: "never".into(),
                    sandbox_mode: "read-only".into(),
                    developer_instructions: None,
                    config: calm_server::shared_codex_appserver::ThreadConfig::NoMcp,
                },
            )
            .await
            .unwrap();
    }
    let rebooted = Boot {
        app: router(&state).await,
        state,
        repo: boot.repo.clone(),
        planner_card_id: boot.planner_card_id.clone(),
        _tmp: TempDir::new().unwrap(),
    };
    (carrier, rebooted)
}

/// Production boot order: harness recovery, then operation recovery.
async fn recover(boot: &Boot) {
    calm_server::recover_harnesses_after_daemon_boot(&boot.state, Ok(()))
        .await
        .unwrap();
    calm_server::recover_operations_on_boot(&boot.state)
        .await
        .unwrap();
}

/// A crash after `TxCommitted` whose re-driven start then fails ends where a live failure after
/// the cutover ends: the new session is the card's failed carrier, the old one stays superseded
/// and is never recovered, resumed or given the card back; restart recovers in place.
#[tokio::test]
async fn planner_new_conversation_a_crash_after_the_commit_fails_on_boot_like_a_live_failure() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let old_thread = old.thread_id.clone().unwrap();
    let (carrier, rebooted) = crash_after_the_commit(&boot, "crashed").await;
    rebooted
        .state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    recover(&rebooted).await;

    let (phase, compensation) = conversation_start(&rebooted).await;
    assert_eq!(phase, "failed", "the re-driven start failed");
    let compensation = compensation.unwrap_or_default();
    assert!(compensation.contains("fail_runtime"), "{compensation}");
    assert!(
        !compensation.contains("restore_old_runtime"),
        "the committed cutover is final: {compensation}"
    );
    assert_eq!(rebooted.current_session().await, Some(carrier.clone()));
    assert_eq!(state_of(&rebooted, &carrier).await, "failed");
    assert_eq!(state_of(&rebooted, &old.id).await, "superseded");
    assert_eq!(rebooted.floors().await, 1, "the floor stays");
    assert!(rebooted.state.harness.get(&old.id).is_none());
    let daemon = &rebooted.state.shared_codex_appserver;
    assert!(
        daemon
            .resumed_threads_for_test()
            .iter()
            .all(|(thread, _)| thread != &old_thread),
        "the old thread is never resumed"
    );
    let (status, body) = rebooted.new_conversation("crashed").await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "the lost request's key answers its failure: {body}"
    );

    let (status, body) = rebooted.fresh_start("restart").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let recovered = rebooted.active().await;
    assert_ne!(recovered.id, old.id);
    assert_ne!(recovered.id, carrier);
    assert_eq!(state_of(&rebooted, &old.id).await, "superseded");
    assert_eq!(rebooted.floors().await, 1, "restart leaves the floor alone");
    rebooted.shutdown().await;
}

/// The same crash with a healthy provider: boot finishes the start, the new conversation is
/// current on a new thread, the lost request's key answers it, and the old session stays retired.
#[tokio::test]
async fn planner_new_conversation_a_crash_after_the_commit_completes_on_boot() {
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let old_thread = old.thread_id.clone().unwrap();
    let (carrier, rebooted) = crash_after_the_commit(&boot, "finished").await;
    recover(&rebooted).await;

    assert_eq!(conversation_start(&rebooted).await.0, "succeeded");
    let current = rebooted.active().await;
    assert_eq!(
        current.id, carrier,
        "the committed session is the one started"
    );
    let thread = current.thread_id.clone().unwrap();
    assert_ne!(thread, old_thread);
    assert!(rebooted.state.harness.get(&carrier).is_some());
    assert!(rebooted.state.harness.get(&old.id).is_none());
    assert_eq!(state_of(&rebooted, &old.id).await, "superseded");
    assert_eq!(rebooted.active_session_rows().await, 1);
    let (status, body) = rebooted.new_conversation("finished").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["conversation"], json!(1));
    assert_eq!(body["new_thread_id"], json!(thread));
    assert_eq!(rebooted.floors().await, 1);
    rebooted.shutdown().await;
}

const FORGE_HASH: &str = "payload-hash";

/// The forge submit's approval for `key`, read as the card's Planner in one transaction.
async fn forge_approval(boot: &Boot, key: &str) -> Option<ForgeApproval> {
    let (track, card) = track_and_card(boot).await;
    let key = key.to_string();
    calm_server::db::write_in_tx_typed(boot.repo.as_ref() as &dyn Repo, move |tx| {
        Box::pin(async move {
            Ok(
                match forge_approval_tx(tx, &track, &card, &key, FORGE_HASH, "Merge?").await? {
                    ForgeApprovalRead::Recorded(approval) => Some(approval),
                    ForgeApprovalRead::Ask(..) => None,
                },
            )
        })
    })
    .await
    .unwrap()
}

/// Raise the approval ask for `key` as the forge gate raises it: the Planner card's own event.
async fn raise_forge_ask(boot: &Boot, key: &str) -> i64 {
    assert_eq!(forge_approval(boot, key).await, None, "premise: no ask yet");
    let (track, card) = track_and_card(boot).await;
    let key = key.to_string();
    let (_, ids) = calm_server::db::write_with_actor_events_typed::<(), _>(
        boot.repo.as_ref() as &dyn Repo,
        None,
        &boot.state.events,
        boot.state.write(),
        move |tx| {
            Box::pin(async move {
                let ForgeApprovalRead::Ask(scope, event) =
                    forge_approval_tx(tx, &track, &card, &key, FORGE_HASH, "Merge?").await?
                else {
                    panic!("no ask yet");
                };
                Ok(((), vec![(ActorId::AiPlanner(card), scope, *event)]))
            })
        },
    )
    .await
    .unwrap();
    ids[0]
}

async fn track_and_card(boot: &Boot) -> (TrackId, CardId) {
    let card = boot
        .repo
        .card_get(&boot.planner_card_id)
        .await
        .unwrap()
        .unwrap();
    (card.track_id.clone(), card.id)
}

async fn approve(boot: &Boot, ask_id: i64) {
    let (track, _) = track_and_card(boot).await;
    let (status, body) = request(
        person(boot, LEGACY_OWNER_USER_ID),
        "POST",
        format!("/api/tracks/{track}/asks/{ask_id}/answer"),
        json!({"answers": [{"option": 0}]}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");
}

fn turns_on(boot: &Boot, thread: &str) -> usize {
    boot.state
        .shared_codex_appserver
        .started_turns_for_test()
        .iter()
        .filter(|(on, _)| on == thread)
        .count()
}

/// Forge approvals are bound to the card, not the session: one approved before the cutover is
/// still Approved after it, and one still open is answered after it, waking the new session.
#[tokio::test]
async fn planner_new_conversation_keeps_forge_approvals_bound_to_the_card() {
    const APPROVED: &str = "gitforge:merge:approved-before";
    const OPEN: &str = "gitforge:merge:answered-after";
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let old_thread = old.thread_id.clone().unwrap();
    let approved = raise_forge_ask(&boot, APPROVED).await;
    approve(&boot, approved).await;
    assert_eq!(
        forge_approval(&boot, APPROVED).await,
        Some(ForgeApproval::Approved)
    );
    // The answer wakes the old conversation; let it settle so admission finds it idle.
    finish_turn(&boot, &old.id, None).await;
    let open = raise_forge_ask(&boot, OPEN).await;

    let (status, body) = boot.new_conversation(&new_id()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an open wake ask does not block: {body}"
    );
    let new = boot.active().await;
    let new_thread = new.thread_id.clone().unwrap();
    assert_eq!(
        forge_approval(&boot, APPROVED).await,
        Some(ForgeApproval::Approved),
        "the identical call is still approved"
    );
    assert_eq!(
        forge_approval(&boot, OPEN).await,
        Some(ForgeApproval::Awaiting { ask_id: open }),
        "the open ask is the same ask, not raised again"
    );

    let old_turns = turns_on(&boot, &old_thread);
    approve(&boot, open).await;
    assert_eq!(
        forge_approval(&boot, OPEN).await,
        Some(ForgeApproval::Approved)
    );
    wait_for("the answer wakes the new session", async || {
        turns_on(&boot, &new_thread) > 0
    })
    .await;
    assert_eq!(
        turns_on(&boot, &old_thread),
        old_turns,
        "and not the old one"
    );
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// A turn of the earlier conversation is read-only history: replacing it is refused and changes
/// nothing, in either conversation.
#[tokio::test]
async fn planner_new_conversation_refuses_to_replace_an_old_turn() {
    use crate::support::planner_queue_fixture::post_input_keyed_as;
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let (status, body) = post_input(
        boot.app.clone(),
        &boot.input_uri(),
        "a message of the old conversation",
        &new_id(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let turn = finish_turn(&boot, &old.id, Some("the old reply")).await;
    let history = || async {
        let (status, rows) = get(
            boot.app.clone(),
            boot.card_uri("harness/items?conversation=0"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={rows}");
        rows
    };
    let (_, before) = get(boot.app.clone(), boot.card_uri("harness/items")).await;
    assert!(
        before
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["turn_id"] == json!(turn)),
        "premise: the turn is in the transcript: {before}"
    );

    let (status, body) = boot.new_conversation(&new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let new = boot.active().await;
    let old_rows = history().await;
    assert_eq!(old_rows, before, "premise: the old turn is history");
    let bindings = boot.bindings().await;

    let (status, body) = post_input_keyed_as(
        boot.app.clone(),
        &boot.planner_card_id,
        json!({"text": "edited after the cutover", "replaces_turn": turn}),
        &new_id(),
        "user",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_turn_not_replaceable"),
        "{body}"
    );
    assert_eq!(history().await, old_rows, "the history is unchanged");
    let (_, current) = get(boot.app.clone(), boot.card_uri("harness/items")).await;
    assert_eq!(current, json!([]), "the new conversation stays blank");
    assert_eq!(boot.bindings().await, bindings, "the send stored nothing");
    let pending = boot
        .state
        .harness
        .get(&new.id)
        .unwrap()
        .snapshot()
        .await
        .unwrap()
        .pending_entries();
    assert!(pending.is_empty(), "nothing was queued: {pending:?}");
    boot.shutdown_session(&old.id).await;
    boot.shutdown().await;
}

/// A Claude Planner's token is cleared by the cutover: the old token never authenticates again,
/// and the new conversation's first turn mints one that does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planner_new_conversation_refuses_a_claude_planners_old_token() {
    use crate::claude_planner_stack_fixture::{Root, Stack};
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let (outcome, token) = stack.run_turn(&root, &card_id, "exit", "hello").await;
    assert_eq!(outcome["status"], "completed");
    assert!(stack.token_authenticates(&token).await, "premise");
    let old = stack.runtime(&card_id).await;

    let (status, body) = stack
        .send_with(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{card_id}/planner/conversations"))
                .header("x-calm-actor", "user")
                .header("idempotency-key", new_id()),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let new = stack.runtime(&card_id).await;
    assert_ne!(new.id, old.id);
    assert!(
        !stack.token_authenticates(&token).await,
        "the old token is refused"
    );
    assert_eq!(stack.row_hash(&old.id).await, None, "its hash is cleared");

    let (outcome, new_token) = stack
        .run_turn(&root, &card_id, "exit", "in the new conversation")
        .await;
    assert_eq!(outcome["status"], "completed");
    assert_ne!(new_token, token);
    assert!(stack.token_authenticates(&new_token).await);
    assert!(!stack.token_authenticates(&token).await);
    stack.shutdown().await;
}

/// An event committed on the new process while the committed start is still `starting`, before
/// boot re-drives it, finds no request holding the card and no harness to push to; the new
/// session's install catches up from the moved watermark and delivers it exactly once.
#[tokio::test]
async fn planner_new_conversation_an_event_before_the_boot_redrive_arrives_once() {
    use crate::planner_new_conversation_wakes::{arrives_once, wake};
    const WAKE: &str = "committed on the new process before the start was re-driven";
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let old_thread = old.thread_id.clone().unwrap();
    let (carrier, rebooted) = crash_after_the_commit(&boot, "redriven").await;
    calm_server::recover_harnesses_after_daemon_boot(&rebooted.state, Ok(()))
        .await
        .unwrap();
    assert!(
        rebooted.state.harness.get(&carrier).is_none(),
        "premise: nothing serves the starting row"
    );
    wake(&rebooted, WAKE).await;
    // Let the dispatcher find the row `starting` before the start is re-driven.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    calm_server::recover_operations_on_boot(&rebooted.state)
        .await
        .unwrap();
    assert_eq!(rebooted.active().await.id, carrier);
    arrives_once(&rebooted, &carrier, &old_thread, WAKE).await;
    rebooted.shutdown().await;
}

/// The re-driven start's install persisted its catch-up and was then lost before the start
/// succeeded: the next drive takes the queue and the watermark from the same row, so the event
/// it already caught up on is not queued, or delivered, a second time.
#[tokio::test]
async fn planner_new_conversation_a_drive_lost_after_its_catch_up_delivers_once() {
    use crate::planner_new_conversation_wakes::{arrives_once, wake};
    const WAKE: &str = "caught up by a drive that was then lost";
    let boot = ordinary_track().await;
    let old = boot.active().await;
    let old_thread = old.thread_id.clone().unwrap();
    let (carrier, rebooted) = crash_after_the_commit(&boot, "caught-up").await;
    calm_server::recover_harnesses_after_daemon_boot(&rebooted.state, Ok(()))
        .await
        .unwrap();
    let event = wake(&rebooted, WAKE).await;
    // Let the dispatcher find the row `starting` before the start is re-driven.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let (entered, _release) = pause_at(CAUGHT_UP, &rebooted.planner_card_id);
    let drive = {
        let state = rebooted.state.clone();
        tokio::spawn(async move { calm_server::recover_operations_on_boot(&state).await })
    };
    entered.notified().await;
    drive.abort();
    let _ = drive.await;
    let row = calm_server::harness::HarnessSnapshot::from_value_strict(
        rebooted.active().await.handle_state_json.unwrap(),
    );
    assert!(
        row.push_watermark >= event,
        "premise: the catch-up persisted"
    );
    assert_eq!(
        row.pending_entries()
            .iter()
            .filter(|entry| format!("{:?}", entry.observation()).contains(WAKE))
            .count(),
        1,
        "premise: the row holds the event once"
    );
    assert_eq!(
        conversation_start(&rebooted).await.0,
        "spawn_started",
        "premise"
    );

    calm_server::recover_operations_on_boot(&rebooted.state)
        .await
        .unwrap();
    assert_eq!(conversation_start(&rebooted).await.0, "succeeded");
    assert_eq!(rebooted.active().await.id, carrier);
    arrives_once(&rebooted, &carrier, &old_thread, WAKE).await;
    rebooted.shutdown().await;
}
