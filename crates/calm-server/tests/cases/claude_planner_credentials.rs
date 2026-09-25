//! #1791 PR4 must-reds on the Claude Planner's credential and boot lifecycle (§5.1 items 1, 2, 5):
//! boot revokes before the listener opens and sweeps every Claude Planner marker, the first turn
//! mints under the issuance lock, a superseded row never authenticates again, and boot records the
//! lost turn. Each test runs the production boot on a private root.

use std::time::Duration;

use axum::http::StatusCode;
use calm_server::claude_planner::stop::{
    clear_claude_planner_stop_failure_for_test, fail_claude_planner_stop_for_test,
};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::session_set_handle_state_tx;
use calm_server::harness::{HarnessPhaseTag, HarnessSnapshot, QueueEntry};
use calm_server::operation::planner_harness_start_adapter::claude_spawn_failure;
use calm_server::session_projection_repo::AgentProvider;
use serde_json::{Value, json};

use super::claude_planner_stack_fixture::{Root, Stack};

const BUDGET: Duration = Duration::from_secs(20);

/// §9.1 PR4: the provider persists through start, reset and boot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_provider_persists_through_start_reset_and_boot() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let started = stack.runtime(&card_id).await;
    assert_eq!(started.agent_provider, Some(AgentProvider::Claude));
    assert_eq!(stack.harness(&started.id).provider(), AgentProvider::Claude);

    let (status, body) = stack.reset(&card_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reset = stack.runtime(&card_id).await;
    assert_ne!(reset.id, started.id);
    assert_eq!(reset.agent_provider, Some(AgentProvider::Claude));
    assert_eq!(stack.harness(&reset.id).provider(), AgentProvider::Claude);
    let (outcome, _) = stack.run_turn(&root, &card_id, "exit", "after reset").await;
    assert_eq!(outcome["status"], "completed");

    stack.shutdown().await;
    let stack = Stack::boot(&root).await;
    let booted = stack.runtime(&card_id).await;
    assert_eq!(booted.id, reset.id);
    assert_eq!(booted.agent_provider, Some(AgentProvider::Claude));
    assert_eq!(stack.harness(&booted.id).provider(), AgentProvider::Claude);
}

/// §9.1 PR4: the Codex daemon down at boot still recovers a Claude Planner (the fixture's daemon
/// is always down, so this is the boot recovery's `Err` arm).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_codex_daemon_down_at_boot_still_recovers_claude() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    stack.shutdown().await;

    let stack = Stack::boot(&root).await;
    assert!(
        stack.state.harness.get(&runtime.id).is_some(),
        "recovered at boot, before any input"
    );
    let (outcome, _) = stack.run_turn(&root, &card_id, "exit", "hello").await;
    assert_eq!(outcome["status"], "completed");
}

/// §9.1 PR4: a reconnect with the old token from the moment the listener starts fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconnect_with_the_old_token_from_listener_start_fails() {
    use crate::support::mcp::{connect, recv_frame, send_frame};
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let (_, token) = stack.run_turn(&root, &card_id, "exit", "hello").await;
    assert!(stack.token_authenticates(&token).await);
    stack.shutdown().await;

    let stack = Stack::boot(&root).await;
    let socket = stack
        .state
        .mcp_server
        .as_ref()
        .expect("listener")
        .shim_config
        .socket_path
        .clone();
    let (mut rd, mut wr) = connect(&socket).await;
    send_frame(
        &mut wr,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "reconnect", "version": "0"},
            "_meta": {"dev.neige/auth": {"token": token}}}}),
    )
    .await;
    let reply = recv_frame(&mut rd).await;
    assert!(
        reply.get("error").is_some(),
        "the old token was revoked: {reply}"
    );
}

/// §9.1 PR4: a superseded Claude row whose marked process survived (a crash after the reset
/// commit) has none after boot; a delete after that boot finds none either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_superseded_claude_rows_live_process_is_gone_after_boot_and_delete() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    let old = stack.runtime(&card_id).await;
    let (status, body) = stack.reset(&card_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    stack.shutdown().await;
    // The reset committed, then the server died before the old id's stop: its Bash lives on.
    root.spawn_marked_orphan(&old.id);
    assert_eq!(root.marked_pids(&old.id).len(), 1);

    let stack = Stack::boot(&root).await;
    assert!(
        root.wait_unmarked(&old.id, BUDGET).await.is_empty(),
        "boot swept the superseded row's marker"
    );
    let (status, body) = stack
        .send("DELETE", &format!("/api/tracks/{track_id}"), None)
        .await;
    assert!(status.is_success(), "{status} {body}");
    assert!(root.marked_pids(&old.id).is_empty());
}

/// §9.1 PR4: after boot, a reset of a Claude card whose harness was not reinstalled never
/// accepts the old token (boot nulls session hashes; `card_mcp_tokens` still holds the old hash,
/// which the card-hash mirror must not copy onto the reset's row).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_boot_a_reset_without_a_reinstalled_harness_never_accepts_the_old_token() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let (_, token) = stack.run_turn(&root, &card_id, "exit", "hello").await;
    stack.shutdown().await;

    let stack = Stack::boot(&root).await;
    let runtime = stack.runtime(&card_id).await;
    if let Some(handle) = stack.state.harness.remove(&runtime.id) {
        handle.shutdown().await.expect("shutdown");
    }
    assert!(!stack.token_authenticates(&token).await);
    let (status, body) = stack.reset(&card_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !stack.token_authenticates(&token).await,
        "the reset's row must not inherit the card's old hash"
    );
    let (outcome, new_token) = stack.run_turn(&root, &card_id, "exit", "after").await;
    assert_eq!(outcome["status"], "completed");
    assert!(stack.token_authenticates(&new_token).await);
    assert!(!stack.token_authenticates(&token).await);
}

/// §9.1 PR4: an aborted deletion. With the stop seam armed the delete aborts before anything moves
/// and the harness is reinstalled Live; the old token is rejected with no new input; its first
/// turn fails until the seam clears; then a queued input mints, the turn succeeds and no marked
/// process is left.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_aborted_deletion_revokes_and_the_reinstalled_harness_mints_on_its_first_turn() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    let (_, token) = stack.run_turn(&root, &card_id, "exit", "hello").await;
    let runtime = stack.runtime(&card_id).await;

    fail_claude_planner_stop_for_test(&runtime.id);
    let (status, body) = stack
        .send("DELETE", &format!("/api/tracks/{track_id}"), None)
        .await;
    assert!(!status.is_success(), "the delete aborts: {status} {body}");
    assert!(stack.repo().track_get(&track_id).await.unwrap().is_some());
    let reinstalled = stack.state.harness.get(&runtime.id).expect("Live again");
    assert!(
        !stack.token_authenticates(&token).await,
        "revoked with no new input"
    );

    let spawns_before = root.read_fake("spawns").unwrap_or_default().lines().count();
    let (status, _) = stack.post_input(&card_id, "after the abort").await;
    assert_eq!(status, StatusCode::OK);
    let mut refused = 0;
    for _ in 0..200 {
        refused = reinstalled.refused_issuances_for_test();
        if refused > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        refused > 0,
        "the first turn's setup stop fails under the seam"
    );
    assert_eq!(
        root.read_fake("spawns").unwrap_or_default().lines().count(),
        spawns_before,
        "nothing spawned while the seam holds"
    );
    assert_eq!(stack.outcomes(&card_id).await.len(), 1);

    clear_claude_planner_stop_failure_for_test(&runtime.id);
    let outcomes = stack.wait_outcomes(&card_id, 2).await;
    assert_eq!(outcomes[1]["status"], "completed", "{outcomes:?}");
    let new_token = root.spawned_token();
    assert!(stack.token_authenticates(&new_token).await);
    assert!(!stack.token_authenticates(&token).await);
    assert!(root.wait_unmarked(&runtime.id, BUDGET).await.is_empty());
}

/// §9.1 PR4: a Claude reset whose post-commit step fails (the one-shot spawn failure, held at an
/// awaitable pause after the reset commit): the old token is rejected at the pause and after
/// compensation restores the old row; then an input hits the registry miss, the respawned
/// harness's first turn mints and succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_claude_reset_never_lets_the_old_token_back() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let (_, token) = stack.run_turn(&root, &card_id, "exit", "hello").await;
    let old = stack.runtime(&card_id).await;

    let failure = claude_spawn_failure::arm(&card_id);
    let reset = {
        let app = stack.app.clone();
        let card_id = card_id.clone();
        tokio::spawn(async move {
            use axum::body::Body;
            use axum::http::Request;
            use tower::ServiceExt;
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/cards/{card_id}/planner/reset"))
                    .header("x-calm-actor", "user")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        })
    };
    tokio::time::timeout(BUDGET, failure.entered.notified())
        .await
        .expect("the reset reached the spawn");
    assert!(
        !stack.token_authenticates(&token).await,
        "at the pause (after the reset commit) the old token is rejected"
    );
    failure.release.notify_one();
    let status = reset.await.expect("reset task");
    assert!(
        !status.is_success(),
        "the injected failure fails the reset: {status}"
    );
    let restored = stack.runtime(&card_id).await;
    assert_eq!(restored.id, old.id, "compensation restored the old row");
    assert!(
        !stack.token_authenticates(&token).await,
        "the restored row does not carry the old hash"
    );

    let before = stack.outcomes(&card_id).await.len();
    root.set_scenario("exit");
    let (status, body) = stack.post_input(&card_id, "after the failed reset").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcomes = stack.wait_outcomes(&card_id, before + 1).await;
    assert_eq!(outcomes[before]["status"], "completed", "{outcomes:?}");
    let new_token = root.spawned_token();
    assert_ne!(new_token, token);
    assert!(stack.token_authenticates(&new_token).await);
}

/// Rewrite the Planner row's snapshot as a crash at a given point would have left it.
async fn rewrite_snapshot(
    root: &Root,
    worker_session_id: &str,
    edit: impl FnOnce(&mut HarnessSnapshot),
) {
    let repo = calm_server::db::sqlite::SqlxRepo::open(&root.db_url())
        .await
        .expect("open db");
    let row = repo
        .session_projection_by_id(worker_session_id)
        .await
        .expect("read")
        .expect("row");
    let mut snapshot = HarnessSnapshot::from_value_strict(row.handle_state_json.expect("snapshot"));
    edit(&mut snapshot);
    let mut tx = repo.pool().begin().await.expect("tx");
    session_set_handle_state_tx(
        &mut tx,
        &worker_session_id.to_string(),
        Some(serde_json::to_value(&snapshot).expect("snapshot json")),
    )
    .await
    .expect("write snapshot");
    tx.commit().await.expect("commit");
}

fn outcome_of<'a>(outcomes: &'a [Value], turn_id: &str) -> Vec<&'a Value> {
    outcomes.iter().filter(|o| o["id"] == turn_id).collect()
}

/// §9.1 PR4 (§5.1 D21): a crash AFTER the snapshot commit that made the turn id durable leaves
/// `interrupted` for it and no re-drain; a crash BEFORE it re-drains the batch (the pre-drain
/// snapshot still owns it); a turn that had settled keeps its outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boot_after_a_crash_records_interrupted_or_re_drains_and_keeps_settled_outcomes() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let (settled, _) = stack.run_turn(&root, &card_id, "exit", "first").await;
    let settled_id = settled["id"].as_str().unwrap().to_string();
    let runtime = stack.runtime(&card_id).await;
    stack.shutdown().await;

    // After the commit: the turn id is durable, the batch is gone from the queue.
    rewrite_snapshot(&root, &runtime.id, |snapshot| {
        snapshot.phase = HarnessPhaseTag::TurnRunning;
        snapshot.last_turn_id = Some("turn-lost-in-the-crash".into());
        snapshot.set_pending_entries(Vec::new());
    })
    .await;
    let spawns = root.read_fake("spawns").unwrap_or_default().lines().count();
    let stack = Stack::boot(&root).await;
    let outcomes = stack.outcomes(&card_id).await;
    let lost = outcome_of(&outcomes, "turn-lost-in-the-crash");
    assert_eq!(lost.len(), 1, "{outcomes:?}");
    assert_eq!(lost[0]["status"], "interrupted");
    assert_eq!(
        lost[0]["error"]["message"],
        "neige restarted during this turn"
    );
    assert_eq!(outcome_of(&outcomes, &settled_id)[0]["status"], "completed");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        root.read_fake("spawns").unwrap_or_default().lines().count(),
        spawns,
        "no re-drain"
    );
    stack.shutdown().await;

    // Before the commit: the pre-drain snapshot still holds the batch; the last durable turn is
    // the settled one.
    rewrite_snapshot(&root, &runtime.id, |snapshot| {
        snapshot.phase = HarnessPhaseTag::IssuingTurn;
        snapshot.last_turn_id = Some(settled_id.clone());
        snapshot.set_pending_entries(vec![QueueEntry::user_message(
            "the batch the crash took".into(),
            None,
            Vec::new(),
        )]);
    })
    .await;
    root.remove_fake("stdin");
    let stack = Stack::boot(&root).await;
    let before = outcomes.len();
    let outcomes = stack.wait_outcomes(&card_id, before + 1).await;
    assert_eq!(outcomes[before]["status"], "completed", "{outcomes:?}");
    assert!(
        root.read_fake("stdin")
            .expect("re-drained line")
            .contains("the batch the crash took")
    );
    let settled_rows = outcome_of(&outcomes, &settled_id);
    assert_eq!(settled_rows.len(), 1);
    assert_eq!(
        settled_rows[0]["status"], "completed",
        "the settled outcome is kept"
    );
}
