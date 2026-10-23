//! #1791 PR4 must-reds on retiring a Claude Planner (§5.1 items 2–4): reset, deletion, workspace
//! repoint, the stop-failure seam, an install-race loser and a stale harness after its shutdown.
//! Each test runs the production boot on a private root (`claude_planner_stack_fixture.rs`).

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use calm_server::db::prelude::*;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, PlannerBackend, PlannerHarness,
    PlannerHarnessParams, QueueEntry,
};
use provider::InputItem;
use provider::TurnModelSelection;
use serde_json::json;

use super::claude_planner_session_fixture::{alive, request};
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

/// Repoint refuses a retained live execution, then succeeds after owner completion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_repoint_refuses_the_live_execution_until_owner_completion() {
    let root = Root::new("hold");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    let before = stack
        .repo()
        .track_get(&track_id)
        .await
        .unwrap()
        .unwrap()
        .workspace;
    assert_eq!(stack.post_input(&card_id, "work").await.0, StatusCode::OK);
    wait_file(&root, "stdin").await;
    let pid = fake_pid(&root);
    assert!(alive(pid));
    let target = user_repo(&root.path().join("user-repo"));
    let (status, body) = repoint(&stack, &track_id, &target).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        stack
            .repo()
            .track_get(&track_id)
            .await
            .unwrap()
            .unwrap()
            .workspace,
        before
    );
    assert!(alive(pid), "the retained carrier is still live");
    let runtime = stack.runtime(&card_id).await;
    let host = stack.state.planner_wiring().claude;
    let session = stack
        .harness(&runtime.id)
        .claude_session_for_test()
        .unwrap();
    session.abort_driver_for_test().await;
    let pool = stack.repo().sqlite_db().unwrap();
    let claim = calm_truth::db::sqlite::provider_execution::load(pool.read_pool(), &runtime.id)
        .await
        .unwrap()
        .unwrap();
    let id = calm_server::provider_execution::execution_id(claim.record()).unwrap();
    host.provider_execution
        .stop(
            &pool,
            host.execution.engine(),
            &runtime.id,
            &claim,
            calm_server::provider_execution::StopReason::DriverLost,
        )
        .await
        .unwrap();
    calm_server::provider_execution::wait_settled(
        host.execution.engine().unwrap(),
        &id,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert!(
        calm_truth::db::sqlite::provider_execution::load(pool.read_pool(), &runtime.id)
            .await
            .unwrap()
            .is_some(),
        "settled execution still needs owner completion"
    );
    let (status, body) = repoint(&stack, &track_id, &target).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!alive(pid), "Engine settled before repoint");
    assert_eq!(
        stack.outcomes(&card_id).await.len(),
        1,
        "preflight recorded the lost turn before replacement"
    );
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
    stack.shutdown().await;
}

/// R2 keeps a live owner current through completion; reset succeeds only after claim clearance.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reset_refuses_a_running_owner_then_succeeds_after_completion() {
    let root = Root::new("hold");
    let stack = Stack::boot(&root).await;
    let (_, card_id) = stack.create_claude_track().await;
    let old = stack.runtime(&card_id).await;
    assert_eq!(stack.post_input(&card_id, "work").await.0, StatusCode::OK);
    wait_file(&root, "stdin").await;
    let pid = fake_pid(&root);
    assert!(alive(pid));
    assert!(!root.instructions_files().is_empty());
    let (status, body) = stack.reset(&card_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(stack.runtime(&card_id).await.id, old.id);
    assert!(alive(pid), "R2 retains the live carrier");
    stack
        .harness(&old.id)
        .interrupt("finish before reset".into())
        .await
        .unwrap();
    stack.wait_outcomes(&card_id, 1).await;
    stack.wait_provider_clear(&old.id).await;
    let (status, body) = stack.reset(&card_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!alive(pid));
    assert!(root.instructions_files().is_empty());
    stack.shutdown().await;
}

/// R4 deletion settles the retained execution after R2 refused a live reset.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_reset_then_delete_settles_the_retained_execution() {
    let root = Root::new("hold");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    assert_eq!(stack.post_input(&card_id, "work").await.0, StatusCode::OK);
    wait_file(&root, "stdin").await;
    let pid = fake_pid(&root);
    let old = stack.runtime(&card_id).await;
    let (status, body) = stack.reset(&card_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(stack.runtime(&card_id).await.id, old.id);
    assert!(alive(pid));
    let (status, body) = stack
        .send("DELETE", &format!("/api/tracks/{track_id}"), None)
        .await;
    assert!(status.is_success(), "{status} {body}");
    assert!(
        !alive(pid),
        "R4 settles the retained execution before deleting its row"
    );
    assert!(
        stack
            .repo()
            .session_get(&calm_types::worker::WorkerSessionId(old.id))
            .await
            .unwrap()
            .is_none()
    );
    stack.shutdown().await;
}

/// Failed Engine admission submits nothing and keeps the input queued for retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_engine_admission_keeps_the_input_queued_until_retry() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    let harness = stack.harness(&runtime.id);

    stack.block_provider_admission().await;
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
    assert_eq!(
        harness.snapshot().await.unwrap().pending_entries().len(),
        1,
        "still queued"
    );
    assert!(
        stack.row_hash(&runtime.id).await.is_some(),
        "the credential was minted before the failed Engine admission"
    );

    stack.clear_provider_fault().await;
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
    let host = stack.state.planner_wiring().claude;
    let track = repo.track_get(&track_id).await.unwrap().unwrap();
    let session = Arc::new(
        calm_server::claude_planner::session::ClaudePlannerSession::open(
            calm_server::claude_planner::session::ClaudePlannerSessionParams {
                engine: host.execution.engine().cloned(),
                host: host.clone(),
                worker_session_id: runtime.id.clone(),
                card_id: card_id.clone(),
                track_id: track_id.clone(),
                cwd: track.workspace.agent_cwd().into(),
                instructions: String::new(),
                calm_tools: host.calm_tools.clone(),
                proxy: Vec::new(),
                prior_total_tokens: 0,
                repo: repo.clone(),
                seals: stack.state.thread_seals().clone(),
            },
        )
        .await
        .expect("uninstalled loser session"),
    );
    let mut snapshot = HarnessSnapshot::initial(0, Vec::new());
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = runtime.thread_id.clone();
    snapshot.set_pending_entries(vec![QueueEntry::user_message(
        "the loser's queued input".into(),
        None,
        Vec::new(),
    )]);
    let loser = PlannerHarness::run(PlannerHarnessParams {
        track_delete_locks: Default::default(),
        worker_session_id: runtime.id.clone(),
        track_id: track_id.clone().into(),
        card_id: card_id.clone().into(),
        thread_id: runtime.thread_id.clone(),
        repo,
        events: calm_server::event::EventBus::new(),
        card_role_cache: stack.state.card_role_cache.clone(),
        track_area_cache: stack.state.track_area_cache.clone(),
        backend: PlannerBackend::claude_for_test(session),
        registry: calm_server::harness::HarnessRegistry::new(),
        config: HarnessConfig::default(),
        snapshot: (snapshot)
            .try_into()
            .expect("historical fixture snapshot contract"),
    })
    .await
    .expect("historical fixture harness");
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
            request(
                vec![InputItem::Text {
                    text: "stale".into(),
                }],
                &TurnModelSelection::inherit(),
                calm_server::planner_permission_mode::PlannerPermissionMode::Never,
                &uuid::Uuid::new_v4().simple().to_string(),
            ),
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(calm_server::harness::backend::TurnStartFailure::Transient(
                _
            ))
        ),
        "a shut-down session never starts a turn: {refused:?}"
    );
    assert!(
        stack.token_authenticates(&token).await,
        "the replacer's token stands"
    );
    assert_eq!(card_token_hash(&stack, &card_id).await, card_hash);
}

async fn card_token_hash(stack: &Stack, card_id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT hashed_token FROM card_mcp_tokens WHERE card_id = ?1")
        .bind(card_id)
        .fetch_optional(stack.repo().sqlite_db().expect("pool").read_pool())
        .await
        .expect("card token read")
}

/// §9.1 PR4: a delete that arrives while a spawn is held (after it passed the seal check) does not
/// complete while the held turn can still launch: the delete seals, sweeps, and
/// its harness shutdown waits for the held issuance, which then sees the seal and stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delete_during_a_held_spawn_waits_for_issuance_and_settlement() {
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
    let unsettled: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workspace_executions WHERE settlement_json IS NULL",
    )
    .fetch_one(stack.repo().sqlite_db().unwrap().read_pool())
    .await
    .unwrap();
    assert_eq!(unsettled, 0);
    assert!(stack.repo().track_get(&track_id).await.unwrap().is_none());
}

/// Area deletion revokes before durable Stop; failure preserves the track, retry settles its child.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_area_deletion_revokes_and_sweeps_its_tracks_claude_planners() {
    let root = Root::new("term-ignore");
    let stack = Stack::boot(&root).await;
    let (track_id, card_id) = stack.create_claude_track().await;
    let area_id = stack
        .repo()
        .track_get(&track_id)
        .await
        .unwrap()
        .unwrap()
        .area_id;
    assert_eq!(stack.post_input(&card_id, "work").await.0, StatusCode::OK);
    wait_file(&root, "stdin").await;
    let token = root.spawned_token();
    let pid = fake_pid(&root);
    stack.block_provider_stop().await;
    let (status, body) = stack
        .send("DELETE", &format!("/api/areas/{area_id}"), None)
        .await;
    assert!(!status.is_success(), "{status} {body}");
    assert!(stack.repo().track_get(&track_id).await.unwrap().is_some());
    assert!(
        !stack.token_authenticates(&token).await,
        "revoked before Stop refusal"
    );
    assert!(alive(pid));
    stack.clear_provider_fault().await;
    // Each request has the production quiesce deadline. Under suite load Stop
    // can outlive that request; its durable intent continues and a later retry
    // succeeds. A refusal must keep the track until settlement is confirmed.
    tokio::time::timeout(BUDGET, async {
        loop {
            let (status, body) = stack
                .send("DELETE", &format!("/api/areas/{area_id}"), None)
                .await;
            if status.is_success() {
                break;
            }
            assert_eq!(status, StatusCode::CONFLICT, "{body}");
            assert!(stack.repo().track_get(&track_id).await.unwrap().is_some());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("area deletion succeeds after durable Stop settles");
    assert!(!alive(pid));
    assert!(stack.repo().track_get(&track_id).await.unwrap().is_none());
    stack.shutdown().await;
}
