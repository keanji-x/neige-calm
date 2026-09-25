//! #1791 PR4 must-reds on retiring a Claude Planner (§5.1 items 2–4): reset, deletion, workspace
//! repoint, the stop-failure seam, an install-race loser and a stale harness after its shutdown.
//! Each test runs the production boot on a private root (`claude_planner_stack_fixture.rs`).

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use calm_server::claude_planner::stop::{
    clear_claude_planner_stop_failure_for_test, fail_claude_planner_stop_for_test,
};
use calm_server::claude_planner::wiring::ClaudePlannerRow;
use calm_server::codex_appserver::InputItem;
use calm_server::db::prelude::*;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, PlannerBackend, PlannerHarness,
    PlannerHarnessParams, QueueEntry,
};
use serde_json::json;

use super::claude_planner_stack_fixture::{Root, Stack};
use super::claude_planner_wiring::wait_file;

const BUDGET: Duration = Duration::from_secs(20);

fn fake_pid(root: &Root) -> i32 {
    root.read_fake("pid")
        .expect("pid")
        .trim()
        .parse()
        .expect("pid number")
}

fn spawn_count(root: &Root) -> usize {
    root.read_fake("spawns").unwrap_or_default().lines().count()
}

/// A user-owned git repository to point a track at.
fn user_repo(at: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(at).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "t"],
        vec![
            "commit",
            "-q",
            "--allow-empty",
            "--no-verify",
            "-m",
            "user commit",
        ],
    ] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(at)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }
    at.to_path_buf()
}

async fn repoint(stack: &Stack, track_id: &str, target: &std::path::Path) -> (StatusCode, String) {
    let (status, body) = stack
        .send(
            "PATCH",
            &format!("/api/tracks/{track_id}"),
            Some(json!({"workspace": {
                "kind": "attached",
                "path": target.to_string_lossy(),
                "attach_folder": true,
            }})),
        )
        .await;
    (status, body.to_string())
}

/// §9.1 PR4: a repoint of a Claude track whose marked `setsid` child lives leaves no marked
/// process by the recycle; with `stop` forced to fail it takes the Dirty branch with its own 409
/// and moves nothing; a retried repoint after the seam clears leaves none before the recycle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_repoint_sweeps_the_tracks_claude_planner_processes_or_refuses() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    let before = stack
        .repo()
        .track_get(&track_id)
        .await
        .unwrap()
        .unwrap()
        .workspace;

    root.spawn_marked_orphan(&runtime.id);
    fail_claude_planner_stop_for_test(&runtime.id);
    let target = user_repo(&root.path().join("user-repo"));
    let (status, body) = repoint(&stack, &track_id, &target).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body.contains("a previous Claude Planner process of this track could not be stopped"),
        "{body}"
    );
    assert_eq!(
        stack
            .repo()
            .track_get(&track_id)
            .await
            .unwrap()
            .unwrap()
            .workspace,
        before,
        "nothing moved"
    );
    assert_eq!(
        root.marked_pids(&runtime.id).len(),
        1,
        "the seam signalled nothing"
    );

    clear_claude_planner_stop_failure_for_test(&runtime.id);
    let (status, body) = repoint(&stack, &track_id, &target).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(root.marked_pids(&runtime.id).is_empty());
    assert_eq!(
        stack
            .repo()
            .track_get(&track_id)
            .await
            .unwrap()
            .unwrap()
            .workspace
            .path,
        target.to_string_lossy()
    );
}

/// §9.1 PR4: after a reset during a running turn no old-marker process is alive when the reset
/// returns, and the reset leaves no instructions file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reset_during_a_running_turn_returns_with_the_old_processes_gone() {
    let root = Root::new("hold-orphan");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let old = stack.runtime(&card_id).await;
    let (status, _) = stack.post_input(&card_id, "work").await;
    assert_eq!(status, StatusCode::OK);
    wait_file(&root, "orphan").await;
    wait_file(&root, "stdin").await;
    assert!(!root.marked_pids(&old.id).is_empty());
    assert!(!root.instructions_files().is_empty());

    let (status, body) = stack.reset(&card_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        root.marked_pids(&old.id).is_empty(),
        "no old-marker process when the reset returns"
    );
    assert!(root.instructions_files().is_empty());
}

/// §9.1 PR4: a reset whose `stop` of the old id fails (the seam), then a delete with no boot in
/// between: the delete's scoped sweep leaves no old-id marked process before the move.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_seam_failed_reset_then_delete_leaves_no_old_process() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    stack.run_turn(&root, &card_id, "exit", "hello").await;
    let old = stack.runtime(&card_id).await;
    root.spawn_marked_orphan(&old.id);

    fail_claude_planner_stop_for_test(&old.id);
    let (status, body) = stack.reset(&card_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        root.marked_pids(&old.id).len(),
        1,
        "the failed stop is left behind"
    );
    clear_claude_planner_stop_failure_for_test(&old.id);

    let (status, body) = stack
        .send("DELETE", &format!("/api/tracks/{track_id}"), None)
        .await;
    assert!(status.is_success(), "{status} {body}");
    assert!(root.marked_pids(&old.id).is_empty());
}

/// §9.1 PR4: with the stop seam armed on a fresh harness, its first turn's setup `stop` fails:
/// `turn_start` answers within a bound, the input stays queued, nothing is spawned, and the input
/// is issued once the seam clears (the harness is not a dead handle).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_seam_failed_first_turn_keeps_the_input_queued_until_the_seam_clears() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    let harness = stack.harness(&runtime.id);

    fail_claude_planner_stop_for_test(&runtime.id);
    let (status, _) = stack.post_input(&card_id, "queued").await;
    assert_eq!(status, StatusCode::OK);
    let asked = std::time::Instant::now();
    while harness.refused_issuances_for_test() == 0 {
        assert!(
            asked.elapsed() < Duration::from_secs(10),
            "no refusal within the bound"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(spawn_count(&root), 0);
    assert!(stack.outcomes(&card_id).await.is_empty());
    // The refusal counter is bumped before the drained batch is put back at the head of the
    // queue, so the re-buffer is awaited here (bounded) rather than read once.
    let mut queued = 0;
    for _ in 0..200 {
        queued = harness.snapshot().await.pending_entries().len();
        if queued == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(queued, 1, "still queued");
    assert!(
        stack.row_hash(&runtime.id).await.is_some(),
        "the credential was minted once, before the failing setup stop"
    );

    clear_claude_planner_stop_failure_for_test(&runtime.id);
    let outcomes = stack.wait_outcomes(&card_id, 1).await;
    assert_eq!(outcomes[0]["status"], "completed");
    assert!(root.wait_unmarked(&runtime.id, BUDGET).await.is_empty());
}

/// §9.1 PR4: an install-race loser with a queued input: its turns are refused before any mint or
/// spawn, and its shutdown returns within a bound and signals nothing (its id is the winner's,
/// whose turn keeps running).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_install_race_loser_shuts_down_promptly_and_signals_nothing() {
    let root = Root::new("hold");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    let (status, _) = stack.post_input(&card_id, "the winner's turn").await;
    assert_eq!(status, StatusCode::OK);
    wait_file(&root, "stdin").await;
    let winner_pid = fake_pid(&root);
    let hash = stack.row_hash(&runtime.id).await;
    let spawns = spawn_count(&root);

    let repo: Arc<dyn Repo> = Arc::new(
        calm_server::db::sqlite::SqlxRepo::open(&root.db_url())
            .await
            .expect("repo"),
    );
    let session = stack
        .state
        .claude_planner_wiring()
        .open_session(
            repo.clone(),
            stack.state.shared_codex_appserver.clone(),
            ClaudePlannerRow {
                worker_session_id: &runtime.id,
                card_id: &card_id,
                track_id: &track_id,
                prior_total_tokens: 0,
            },
        )
        .await
        .expect("loser session");
    let mut snapshot = HarnessSnapshot::initial(0, Vec::new());
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = runtime.thread_id.clone();
    snapshot.set_pending_entries(vec![QueueEntry::user_message(
        "the loser's queued input".into(),
        None,
        Vec::new(),
    )]);
    let loser = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime.id.clone(),
        track_id: track_id.clone().into(),
        card_id: card_id.clone().into(),
        thread_id: runtime.thread_id.clone(),
        repo,
        events: calm_server::event::EventBus::new(),
        card_role_cache: stack.state.card_role_cache.clone(),
        track_area_cache: stack.state.track_area_cache.clone(),
        backend: PlannerBackend::Claude(session),
        config: HarnessConfig::default(),
        snapshot,
    });
    let asked = std::time::Instant::now();
    while loser.refused_issuances_for_test() == 0 {
        assert!(
            asked.elapsed() < Duration::from_secs(10),
            "the loser never tried"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::timeout(Duration::from_secs(5), loser.shutdown())
        .await
        .expect("the loser's shutdown is bounded")
        .expect("shutdown");

    assert!(
        super::claude_planner_session_fixture::alive(winner_pid),
        "the winner's turn was not signalled"
    );
    assert_eq!(spawn_count(&root), spawns, "the loser spawned nothing");
    assert_eq!(
        stack.row_hash(&runtime.id).await,
        hash,
        "the loser minted nothing"
    );
}

/// §9.1 PR4: a harness shut down after its install but before its first mint, a replacer that
/// installs and mints, then the stale harness's tick reaching its session: the replacer's token
/// still authenticates and the card hash is unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_harness_tick_after_shutdown_never_mints_over_the_replacer() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    let stale = stack.state.harness.remove(&runtime.id).expect("installed");
    let stale_session = stale.claude_session_for_test().expect("a Claude harness");
    stale.shutdown().await.expect("shutdown before any mint");

    // The replacer: the next input's registry miss respawns the harness, which mints.
    let (_, token) = stack.run_turn(&root, &card_id, "exit", "replacer").await;
    assert!(stack.token_authenticates(&token).await);
    let card_hash = card_token_hash(&stack, &card_id).await;

    let thread = runtime.thread_id.clone().expect("thread");
    let refused = stale_session
        .turn_start(
            &thread,
            vec![InputItem::Text {
                text: "stale".into(),
            }],
            &uuid::Uuid::new_v4().simple().to_string(),
        )
        .await;
    assert!(refused.is_err(), "a shut-down session never starts a turn");
    assert!(
        stack.token_authenticates(&token).await,
        "the replacer's token stands"
    );
    assert_eq!(card_token_hash(&stack, &card_id).await, card_hash);
}

async fn card_token_hash(stack: &Stack, card_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT hashed_token FROM card_mcp_tokens WHERE card_id = ?1")
        .bind(card_id)
        .fetch_optional(&stack.repo().sqlite_pool().expect("pool"))
        .await
        .expect("card token read")
}

/// §9.1 PR4: a delete that arrives while a spawn is held (after it passed the seal check) does not
/// complete while the held turn can still leave a marked process: the delete seals, sweeps, and
/// its harness shutdown waits for the held issuance, which then sees the seal and stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delete_during_a_held_spawn_does_not_complete_while_a_marked_process_lives() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    let session = stack
        .harness(&runtime.id)
        .claude_session_for_test()
        .expect("a Claude harness");
    let gate = Arc::new((Mutex::new(true), Condvar::new()));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let entered_tx = Mutex::new(entered_tx);
    let hook_gate = Arc::clone(&gate);
    session.set_after_spawn_hook_for_test(Arc::new(move || {
        let _ = entered_tx.lock().unwrap().send(());
        let (held, released) = &*hook_gate;
        let mut held = held.lock().unwrap();
        while *held {
            held = released.wait(held).unwrap();
        }
    }));
    let (status, _) = stack.post_input(&card_id, "held").await;
    assert_eq!(status, StatusCode::OK);
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(BUDGET))
        .await
        .unwrap()
        .expect("the spawn reached the hold");

    let delete = {
        let app = stack.app.clone();
        let track_id = track_id.clone();
        tokio::spawn(async move {
            use axum::body::Body;
            use axum::http::Request;
            use tower::ServiceExt;
            app.oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/tracks/{track_id}"))
                    .header("x-calm-actor", "user")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        })
    };
    tokio::time::sleep(Duration::from_secs(2)).await;
    let waited = !delete.is_finished();
    // Released before asserting, so a red run does not leave a worker thread parked in the hook.
    {
        let (held, released) = &*gate;
        *held.lock().unwrap() = false;
        released.notify_all();
    }
    assert!(waited, "the delete waits for the held issuance");
    let status = tokio::time::timeout(BUDGET, delete)
        .await
        .expect("the delete completes once released")
        .expect("delete task");
    assert!(status.is_success(), "{status}");
    assert!(root.marked_pids(&runtime.id).is_empty());
    assert!(stack.repo().track_get(&track_id).await.unwrap().is_none());
}

/// #1791 PR4 review: an area deletion revokes then sweeps the Claude Planner ids of its tracks, in
/// any state, before anything moves: with the stop seam armed it aborts with the credentials
/// revoked and the harness reinstalled (which mints on its next turn); retried, it leaves no
/// marked process of a superseded row behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_area_deletion_revokes_and_sweeps_its_tracks_claude_planners() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    let area_id = stack
        .repo()
        .track_get(&track_id)
        .await
        .unwrap()
        .unwrap()
        .area_id;
    let old = stack.runtime(&card_id).await;
    let (status, body) = stack.reset(&card_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, token) = stack.run_turn(&root, &card_id, "exit", "hello").await;
    let current = stack.runtime(&card_id).await;
    root.spawn_marked_orphan(&old.id);

    fail_claude_planner_stop_for_test(&current.id);
    let (status, body) = stack
        .send("DELETE", &format!("/api/areas/{area_id}"), None)
        .await;
    assert!(
        !status.is_success(),
        "the area delete aborts: {status} {body}"
    );
    assert!(stack.repo().track_get(&track_id).await.unwrap().is_some());
    assert!(
        !stack.token_authenticates(&token).await,
        "revoked before the abort"
    );
    assert!(
        stack.state.harness.get(&current.id).is_some(),
        "reinstalled"
    );
    assert_eq!(
        root.marked_pids(&old.id).len(),
        1,
        "the seam signalled nothing"
    );

    clear_claude_planner_stop_failure_for_test(&current.id);
    let (outcome, new_token) = stack.run_turn(&root, &card_id, "exit", "again").await;
    assert_eq!(outcome["status"], "completed");
    assert!(stack.token_authenticates(&new_token).await);

    let (status, body) = stack
        .send("DELETE", &format!("/api/areas/{area_id}"), None)
        .await;
    assert!(status.is_success(), "{status} {body}");
    assert!(
        root.marked_pids(&old.id).is_empty(),
        "the superseded row's process is gone"
    );
    assert!(root.marked_pids(&current.id).is_empty());
}
