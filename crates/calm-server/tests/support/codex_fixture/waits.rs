//! Dispatcher wiring, task planning, and the waits and budgets the scenarios poll with.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use calm_server::dispatcher::Dispatcher;
use calm_server::ids::ActorId;
use calm_server::operation::TxOutput;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

use super::super::agent_diag::panic_with_agent_diag;
use super::super::git_helpers::{git_stdout, git_stdout_no_cwd, is_hex_sha};
use super::super::planner_turn::planner_identity;

use super::*;

pub fn spawn_dispatcher(fx: &Fixture) -> Dispatcher {
    Dispatcher::spawn_with_terminal_renderer_and_operation_runtime(
        fx.repo_dyn.clone(),
        fx.events.clone(),
        fx.write.clone(),
        fx.codex.clone(),
        fx.daemon.clone(),
        fx.renderer.clone(),
        Some(fx.server.clone()),
        fx.shared.clone(),
        fx.runtime.clone(),
        4,
    )
}

/// Like [`spawn_dispatcher`], but wired to the fixture's `HarnessRegistry` so planner
/// wake-ups reach the real planner session (a fresh registry silently severs them).
pub fn spawn_dispatcher_with_harness(fx: &Fixture) -> Dispatcher {
    Dispatcher::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
        fx.repo_dyn.clone(),
        fx.events.clone(),
        fx.write.clone(),
        fx.codex.clone(),
        fx.daemon.clone(),
        fx.renderer.clone(),
        Some(fx.server.clone()),
        fx.harness.clone(),
        fx.shared.clone(),
        fx.runtime.clone(),
        calm_server::per_card_lock::new_per_card_locks(),
        4,
        std::env::temp_dir().join("neige-test-gate-logs"),
        calm_server::scheduler::WorkerLiveness::DEFAULT,
        calm_server::provider_registry::WorkerProviderRegistry::for_daemon(
            &fx.daemon,
            fx.shared.clone(),
            fx.harness.clone(),
        ),
    )
}

pub async fn plan_codex_task(fx: &Fixture, key: &str, goal: &str) {
    fx.used_injected_plan.store(true, Ordering::SeqCst);
    super::super::report_writes::upsert_block(
        &fx.ctx,
        &fx.registry,
        planner_identity(fx),
        json!({
            "kind": "task",
            "payload": {
                "key": key,
                "kind": "codex",
                "goal": goal,
                "context": { "from": "codex-forge-e2e" },
                "acceptance": "FORGE_E2E.md exists with exactly forge-e2e-ok",
                "no_gate_reason": "real-codex forge E2E",
                "ready": true,
                "declared_by": "spec"
            }
        }),
    )
    .await
    .expect("write codex task block");
}

pub fn task_id(fx: &Fixture, key: &str) -> String {
    format!("{}:{key}", fx.track_id.as_str())
}

pub async fn wait_for_worker_success(
    fx: &Fixture,
    task_id: &str,
    budget: Duration,
) -> OperationRow {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(reason) = task_failed_reason(&fx.repo, task_id).await {
            panic_with_agent_diag(
                fx,
                format!("task {task_id} failed before worker write assertion: {reason}"),
            )
            .await;
        }
        if let Some(worker) = worker_operation_for_task(&fx.repo, task_id).await {
            if worker.phase == "succeeded" {
                return worker;
            }
            if worker.phase == "failed" || worker.phase == "stuck" {
                panic_with_agent_diag(
                    fx,
                    format!(
                        "codex-worker operation for task {task_id} ended in {}",
                        worker.phase
                    ),
                )
                .await;
            }
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for codex-worker operation for task {task_id} to succeed"
                ),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub async fn assert_worker_wrote_marker_file(fx: &Fixture, worker_cwd: &Path) {
    let marker = worker_cwd.join("FORGE_E2E.md");
    assert!(
        marker.is_file(),
        "FORGE_E2E.md was not written at {}",
        marker.display()
    );
    let contents = std::fs::read_to_string(&marker)
        .unwrap_or_else(|e| panic!("read marker file {}: {e}", marker.display()));
    assert_eq!(
        contents.trim(),
        "forge-e2e-ok",
        "FORGE_E2E.md trimmed content mismatch: {contents:?}",
    );

    let bare_main =
        git_stdout_no_cwd(["--git-dir", path_str(&fx.origin_repo), "rev-parse", "main"]);
    assert_eq!(
        bare_main, fx.origin_main_initial,
        "local bare origin main changed; worker must not push"
    );
}

pub async fn assert_worker_commit_landed(
    fx: &Fixture,
    worker_cwd: &Path,
    worker_card_id: &str,
    budget: Duration,
) {
    let row = wait_for_worktree_committed_event(fx, budget).await;
    assert_eq!(row.actor, ActorId::KernelDispatcher);
    assert_eq!(row.scope_kind, "card");
    assert_eq!(row.scope_track.as_deref(), Some(fx.track_id.as_str()));
    assert_eq!(row.scope_card.as_deref(), Some(worker_card_id));
    assert_eq!(row.payload["track_id"], fx.track_id.as_str());
    assert_eq!(row.payload["card_id"], worker_card_id);

    let head = git_stdout(worker_cwd, ["rev-parse", "HEAD"]);
    assert!(
        is_hex_sha(&head),
        "worker worktree HEAD should be a 40-char hex sha, got {head:?}"
    );
    let origin_main = git_stdout(worker_cwd, ["rev-parse", "origin/main"]);
    assert_ne!(
        head, origin_main,
        "worker worktree HEAD should diverge from origin/main after kernel commit"
    );
    assert_eq!(row.payload["commit_sha"], head);
    assert_eq!(
        row.payload["branch"],
        format!("neige/track-{}", fx.track_id.as_str())
    );

    let marker_at_head = git_stdout(worker_cwd, ["show", "HEAD:FORGE_E2E.md"]);
    assert_eq!(
        marker_at_head.trim(),
        "forge-e2e-ok",
        "FORGE_E2E.md content at committed HEAD mismatch"
    );
}

pub async fn wait_for_worktree_committed_event(
    fx: &Fixture,
    budget: Duration,
) -> CommittedEventRow {
    let deadline = Instant::now() + budget;
    loop {
        let rows = committed_event_rows(&fx.repo).await;
        if !rows.is_empty() {
            assert_eq!(
                rows.len(),
                1,
                "expected exactly one worktree.committed event"
            );
            return rows.into_iter().next().expect("one committed event row");
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!("timed out after {budget:?} waiting for worktree.committed"),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub async fn wait_for_first_worktree_committed_event(
    fx: &Fixture,
    task_id: &str,
    budget: Duration,
) -> (i64, CommittedEventRow) {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<RawCommittedEventRowWithId> = sqlx::query_as(
            "SELECT id, actor, scope_kind, scope_track, scope_card, payload \
                 FROM events WHERE kind = 'worktree.committed' ORDER BY id ASC",
        )
        .fetch_all(fx.repo.pool())
        .await
        .expect("worktree.committed event rows");
        if let Some((id, actor, scope_kind, scope_track, scope_card, payload)) =
            rows.into_iter().next()
        {
            return (
                id,
                CommittedEventRow {
                    actor: serde_json::from_str(&actor).expect("event actor json"),
                    scope_kind,
                    scope_track,
                    scope_card,
                    payload: serde_json::from_str(&payload).expect("event payload json"),
                },
            );
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!(
                    "timed out after {budget:?} waiting for first worktree.committed for task {task_id}"
                ),
            )
            .await
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub async fn wait_for_first_forge_event(
    fx: &Fixture,
    kind: &str,
    budget: Duration,
) -> (i64, Option<String>, Value) {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<(i64, Option<String>, String)> = sqlx::query_as(
            "SELECT id, scope_track, payload FROM events WHERE kind = ?1 ORDER BY id ASC",
        )
        .bind(kind)
        .fetch_all(fx.repo.pool())
        .await
        .unwrap_or_else(|e| panic!("{kind} event rows: {e}"));
        if let Some((id, scope_track, payload)) = rows.into_iter().next() {
            return (
                id,
                scope_track,
                serde_json::from_str(&payload).expect("event payload json"),
            );
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(fx, format!("timed out after {budget:?} waiting for {kind}"))
                .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub async fn wait_for_task_completed_id(fx: &Fixture, budget: Duration) -> i64 {
    let deadline = Instant::now() + budget;
    loop {
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT id FROM events WHERE kind = 'task.completed' ORDER BY id ASC")
                .fetch_all(fx.repo.pool())
                .await
                .expect("task.completed event rows");
        if let Some((id,)) = rows.into_iter().next() {
            return id;
        }
        if Instant::now() >= deadline {
            panic_with_agent_diag(
                fx,
                format!("timed out after {budget:?} waiting for task.completed"),
            )
            .await;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub fn output_string(output: &TxOutput, key: &str) -> String {
    output.data[key]
        .as_str()
        .unwrap_or_else(|| panic!("tx_output missing string field {key}: {}", output.data))
        .to_string()
}

pub fn e2e_budget() -> Duration {
    std::env::var("NEIGE_CODEX_FORGE_E2E_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(180))
}

pub fn planner_planning_budget() -> Duration {
    std::env::var("NEIGE_PLANNER_PLANNING_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(240))
}

pub fn capstone_budget() -> Duration {
    std::env::var("NEIGE_CAPSTONE_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(3600))
}

pub fn capstone_stage_budget() -> Duration {
    std::env::var("NEIGE_CAPSTONE_STAGE_BUDGET")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(480))
}
