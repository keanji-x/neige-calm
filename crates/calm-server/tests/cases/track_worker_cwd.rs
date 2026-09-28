//! #1830 S2 — codex and claude workers run in their track's own folder
//! (`docs/architecture/1830-s2-worker-in-track-worktree.md` §6 T1–T5): an attached track's worker
//! runs in its track worktree on `neige/track-<id>`, one at a time, on a clean tree, and every
//! attempt is committed there.
//!
//! The world: the MCP boot's attached Track on a clone of a bare origin, given its track worktree
//! by the production `ensure_track_worktree` (the create route's post-commit step); a runtime with
//! the real `CodexWorkerAdapter` on a fake running Codex daemon, the forge and task-verify
//! adapters; a live Dispatcher and its scheduler. The worker is played by the test: it writes
//! files in the cwd its op froze and reports through the `calm.task.*` MCP tools.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::mcp_server::{McpServer, build_default_registry};
use calm_server::model::{CardRole, Task, TaskStatus};
use calm_server::operation::ProviderAdapter;
use calm_server::operation::codex_adapter::CodexWorkerAdapter;
use calm_server::session_projection_repo::AgentProvider;
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::CodexClient;
use calm_server::track_area_cache::TrackAreaCache;
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use serde_json::{Value, json};

use crate::git_delivery::{Fx, fixture_on_with_runtime, git, ref_target};
use crate::mcp_track_report::{boot, call_tool};
use crate::support::git_helpers::{clone_for_track, init_bare_origin};
use crate::task_recovery::{current, declare};

pub(super) const WAIT: Duration = Duration::from_secs(30);

pub(super) struct World {
    pub(super) fx: Fx,
    pub(super) shared: Arc<SharedCodexAppServer>,
    /// `git worktree list` entries and `refs/heads/neige/*` refs before the track worktree.
    pub(super) before: (usize, usize),
    _mcp: Arc<McpServer>,
    _socket_dir: tempfile::TempDir,
}

/// The attached world (module docs). `.gitignore` names `ignored.log`.
pub(super) async fn world() -> World {
    let boot = boot().await;
    let shared = SharedCodexAppServer::new_fake_running_with_pending(boot.repo.clone(), None);
    let socket_dir = tempfile::Builder::new()
        .prefix("s2-mcp-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let mcp = McpServer::spawn(
        boot.repo.clone(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
        socket_dir.path().join("mcp.sock"),
        PathBuf::from("/nonexistent-shim-bin"),
        build_default_registry(),
        None,
        Arc::new(tokio::sync::OnceCell::new()),
        Arc::new(tokio::sync::OnceCell::new()),
        boot.ctx.gate_logs_dir.clone(),
    )
    .await
    .expect("spawn McpServer");
    let track_areas = TrackAreaCache::new();
    boot.repo.seed_track_area_cache(&track_areas).await.unwrap();
    let before = Arc::new(std::sync::Mutex::new((0, 0)));
    let seen = before.clone();
    let (worker_shared, worker_mcp) = (shared.clone(), mcp.clone());
    let fx = fixture_on_with_runtime(
        boot,
        move |tmp| {
            let origin = tmp.join("origin.git");
            init_bare_origin(&origin, &tmp.join("seed"));
            let checkout = tmp.join("checkout");
            clone_for_track(&origin, &checkout);
            std::fs::write(checkout.join(".gitignore"), "ignored.log\n").unwrap();
            git(&checkout, &["add", ".gitignore"]);
            git(&checkout, &["commit", "-q", "-m", "ignore logs"]);
            git(&checkout, &["push", "-q", "origin", "main"]);
            *seen.lock().unwrap() = (worktree_entries(&checkout), neige_refs(&checkout));
            checkout
        },
        Some(shared.clone()),
        move |boot, workspace_root| {
            let adapter = CodexWorkerAdapter::new(
                boot.repo.clone(),
                Arc::new(CodexClient::new_stub()),
                worker_shared,
                Some(worker_mcp),
                boot.card_role_cache.clone(),
                track_areas,
                workspace_root.to_path_buf(),
            );
            vec![Arc::new(adapter) as Arc<dyn ProviderAdapter>]
        },
    )
    .await;
    let before = *before.lock().unwrap();
    World {
        fx,
        shared,
        before,
        _mcp: mcp,
        _socket_dir: socket_dir,
    }
}

pub(super) fn worktree_entries(repo: &Path) -> usize {
    git(repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count()
}

pub(super) fn neige_refs(repo: &Path) -> usize {
    git(
        repo,
        &["for-each-ref", "--format=%(refname)", "refs/heads/neige/"],
    )
    .lines()
    .filter(|line| !line.is_empty())
    .count()
}

/// Declare an ungated codex task, ready; `extra` merges into the block.
pub(super) async fn declare_task(fx: &Fx, key: &str, extra: Value) {
    let mut block = json!({
        "key": key, "kind": "codex", "goal": format!("work on {key}"),
        "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true,
        "no_gate_reason": "#1830 S2 fixture",
    });
    if let Value::Object(extra) = extra {
        block.as_object_mut().unwrap().extend(extra);
    }
    declare(&fx.boot, block).await;
}

/// Poll the current attempt of `key` until `done` holds.
pub(super) async fn wait_task(fx: &Fx, key: &str, done: impl Fn(&Task) -> bool) -> Task {
    tokio::time::timeout(WAIT, async {
        loop {
            let task = current(&fx.boot, key).await;
            if done(&task) {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "task {key} never reached the state: {:?}",
            current_now(fx, key)
        )
    })
}

fn current_now(fx: &Fx, key: &str) -> String {
    format!("{} / {key}", fx.debug_state())
}

/// A worker the scheduler started: its task `running`, its card, its lease path.
pub(super) struct Started {
    pub(super) task: Task,
    pub(super) identity: ToolCallIdentity,
    pub(super) cwd: PathBuf,
    pub(super) lease_id: String,
    pub(super) base_sha: String,
}

pub(super) async fn wait_running(fx: &Fx, key: &str) -> Started {
    let task = wait_task(fx, key, |task| {
        task.status == TaskStatus::Running && task.worker_card_id.is_some()
    })
    .await;
    let card_id = task.worker_card_id.clone().unwrap();
    let (session_id, thread_id): (String, Option<String>) =
        sqlx::query_as("SELECT id, thread_id FROM worker_sessions WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(&fx.pool())
            .await
            .unwrap();
    let (lease_id, path, base_sha): (String, String, String) = sqlx::query_as(
        "SELECT lease_id, path, base_sha FROM workspace_leases WHERE card_id = ?1 \
         ORDER BY created_at_ms DESC LIMIT 1",
    )
    .bind(&card_id)
    .fetch_one(&fx.pool())
    .await
    .unwrap();
    Started {
        identity: ToolCallIdentity {
            card_id,
            role: CardRole::Worker,
            provider: AgentProvider::Codex,
            session_id,
            track_id: Some(fx.track().to_string()),
            area_id: fx.boot.area_id.as_str().to_string(),
            thread_id: thread_id.unwrap_or_default(),
        },
        cwd: PathBuf::from(path),
        lease_id,
        base_sha,
        task,
    }
}

impl Started {
    pub(super) async fn complete(&self, fx: &Fx) {
        fx.complete(&self.identity, &self.task.id).await;
    }

    pub(super) async fn fail(&self, fx: &Fx, reason: &str) {
        call_tool(
            &fx.boot,
            "calm.task.fail",
            self.identity.clone(),
            json!({"idempotency_key": self.task.id, "reason": reason}),
        )
        .await
        .unwrap();
    }
}

/// `(outcome, settlement)` of the attempt's latest delivery row.
pub(super) async fn delivery_outcome(fx: &Fx, attempt: &str) -> Option<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT outcome, settlement FROM task_git_deliveries WHERE producer_attempt_id = ?1 \
         ORDER BY ordinal DESC LIMIT 1",
    )
    .bind(attempt)
    .fetch_optional(&fx.pool())
    .await
    .unwrap()
}

pub(super) async fn lease_state(fx: &Fx, lease_id: &str) -> String {
    sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
        .bind(lease_id)
        .fetch_one(&fx.pool())
        .await
        .unwrap()
}

/// The candidate commit an attempt's settled delivery pinned.
pub(super) async fn candidate_commit(fx: &Fx, attempt: &str) -> String {
    fx.wait_settled(attempt).await;
    fx.candidate_row(attempt)
        .await
        .unwrap_or_else(|| panic!("attempt {attempt} has no candidate"))
        .commit_sha
}

/// T1 (D6) — a tracked edit and an untracked file refuse the worker with `spawn-failed: refused:
/// track-worktree-dirty`, naming both and not the ignored file; no lease, no worker card. After
/// the Planner commits them, a re-declared task runs on that commit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dirty_worktree_refuses_the_worker_and_lists_the_files() {
    let w = world().await;
    let fx = &w.fx;
    std::fs::write(fx.worktree.join("README.md"), "edited by the Planner\n").unwrap();
    std::fs::write(fx.worktree.join("notes.txt"), "untracked\n").unwrap();
    std::fs::write(fx.worktree.join("ignored.log"), "noise\n").unwrap();
    let cards_before = fx.table_count("cards").await;

    declare_task(fx, "a", json!({})).await;
    let a = wait_task(fx, "a", |task| task.status == TaskStatus::Failed).await;

    let detail = a.status_detail.clone().unwrap_or_default();
    assert!(
        detail.starts_with("spawn-failed: refused: track-worktree-dirty: 2 uncommitted path(s)."),
        "{detail}"
    );
    assert!(
        detail.contains("README.md") && detail.contains("notes.txt"),
        "{detail}"
    );
    assert!(!detail.contains("ignored.log"), "{detail}");
    assert_eq!(fx.table_count("workspace_leases").await, 0, "no lease row");
    assert_eq!(
        fx.table_count("cards").await,
        cards_before,
        "no worker card"
    );

    // The Planner's `git.commit` (`git add -A` + commit in its cwd, the track worktree).
    git(&fx.worktree, &["add", "-A"]);
    git(&fx.worktree, &["commit", "-q", "-m", "planner edits"]);
    let committed = git(&fx.worktree, &["rev-parse", "HEAD"]);
    declare_task(fx, "b", json!({})).await;
    let b = wait_running(fx, "b").await;
    assert_eq!(b.cwd, fx.worktree);
    assert_eq!(
        b.base_sha, committed,
        "the next task starts from the Planner's commit"
    );
}

/// T2 (D7, D8) — a failed attempt is committed on `neige/track-<id>` with a message naming the
/// attempt and `failed`; its delivery row says `failed`, the candidate ref points at the commit
/// and the tree is clean. The next task starts from that commit and reads the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_attempt_is_committed_and_the_next_task_continues_from_it() {
    let w = world().await;
    let fx = &w.fx;
    declare_task(fx, "a", json!({})).await;
    let a = wait_running(fx, "a").await;
    assert_eq!(a.cwd, fx.worktree, "the worker runs in the track worktree");
    std::fs::write(a.cwd.join("a.txt"), "attempt a\n").unwrap();
    a.fail(fx, "could not finish").await;

    let commit = candidate_commit(fx, &a.task.id).await;
    let (outcome, settlement) = delivery_outcome(fx, &a.task.id).await.unwrap();
    assert_eq!(outcome, "failed");
    assert_eq!(settlement.as_deref(), Some("candidate"));
    assert_eq!(git(&fx.worktree, &["rev-parse", "HEAD"]), commit);
    assert_eq!(
        git(&fx.worktree, &["rev-parse", &fx.worker_branch()]),
        commit,
        "the commit is on the track branch"
    );
    let message = git(&fx.worktree, &["log", "-1", "--format=%s"]);
    let row = fx.delivery_row(&a.task.id).await.unwrap();
    assert_eq!(
        message,
        format!(
            "neige: attempt {} failed (delivery {})",
            a.task.id, row.delivery_id
        )
    );
    let candidate = fx.candidate_row(&a.task.id).await.unwrap();
    assert_eq!(
        ref_target(Path::new(&candidate.git_common_dir), &candidate.ref_name).as_deref(),
        Some(commit.as_str())
    );
    assert_eq!(git(&fx.worktree, &["status", "--porcelain"]), "", "clean");

    declare_task(fx, "b", json!({})).await;
    let b = wait_running(fx, "b").await;
    assert_eq!(b.base_sha, commit, "b starts from a's commit");
    assert_eq!(
        std::fs::read_to_string(b.cwd.join("a.txt")).unwrap(),
        "attempt a\n"
    );
}

/// T3 (D1, D2 acceptance) — two tasks, the second after the first settles, both in the track
/// worktree: the repository gains exactly one worktree and one `refs/heads/neige/*` ref over the
/// whole life of the track, and no per-card directory exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attached_track_registers_one_worktree_and_one_branch() {
    let w = world().await;
    let fx = &w.fx;
    for key in ["a", "b"] {
        declare_task(fx, key, json!({})).await;
        let started = wait_running(fx, key).await;
        assert_eq!(started.cwd, fx.worktree, "{key} runs in the track worktree");
        std::fs::write(started.cwd.join(format!("{key}.txt")), key).unwrap();
        started.complete(fx).await;
        candidate_commit(fx, &started.task.id).await;
    }
    assert_eq!(worktree_entries(&fx.track_root), w.before.0 + 1);
    assert_eq!(neige_refs(&fx.track_root), w.before.1 + 1);
    assert!(
        !fx.track_root
            .join(".claude/worktrees")
            .join(fx.track())
            .exists(),
        "no per-card worktree directory"
    );
}

/// T4 (D5 status term): while `a` runs, an independent ready `b` is not claimed, and the report
/// read says why (`trackBusy`, #1830 S2b).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_track_runs_one_worker_at_a_time() {
    let w = world().await;
    let fx = &w.fx;
    let worker = fx.new_worker("a", AgentProvider::Codex).await;
    fx.running_task("a", "codex", &worker.card_id, json!({}))
        .await;
    declare_task(fx, "b", json!({})).await;
    fx.scheduler()
        .schedule_track(fx.boot.track_id.clone())
        .await;
    assert_eq!(current(&fx.boot, "b").await.status, TaskStatus::Pending);
    let read = calm_server::track_report_read::load_report_read_snapshot(
        fx.boot.repo.as_ref(),
        fx.boot.report_card_id.as_str(),
    )
    .await
    .unwrap();
    let reason = read
        .task_diagnostics
        .iter()
        .find(|verdict| verdict.key == "b")
        .and_then(|verdict| verdict.pending_reason.clone());
    assert!(
        matches!(
            reason,
            Some(calm_server::db::sqlite::TaskPendingReason::TrackBusy { .. })
        ),
        "{reason:?}"
    );
}

/// T4b (D5 delivery term): `a` (ungated, so `done`) reports, and a `pre-commit` hook
/// holds its real delivery unsettled; `b` stays `pending`. Once the hook lets go, the delivery
/// settles and `b` is claimed on `a`'s commit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_next_task_waits_for_the_previous_commit() {
    let w = world().await;
    let fx = &w.fx;
    let release = fx.track_root.parent().unwrap().join("release-commit");
    let hook = fx.track_root.join(".git/hooks/pre-commit");
    crate::git_delivery::write_executable(
        &hook,
        &format!(
            "#!/bin/sh\nwhile [ ! -e '{}' ]; do sleep 0.05; done\n",
            release.display()
        ),
    );
    declare_task(fx, "a", json!({})).await;
    let a = wait_running(fx, "a").await;
    std::fs::write(a.cwd.join("a.txt"), "a\n").unwrap();
    a.complete(fx).await;
    wait_task(fx, "a", |task| task.status == TaskStatus::Done).await;

    declare_task(fx, "b", json!({})).await;
    fx.scheduler()
        .schedule_track(fx.boot.track_id.clone())
        .await;
    assert_eq!(current(&fx.boot, "b").await.status, TaskStatus::Pending);
    assert_eq!(delivery_outcome(fx, &a.task.id).await.unwrap().1, None);

    std::fs::write(&release, "").unwrap();
    let commit = candidate_commit(fx, &a.task.id).await;
    let b = wait_running(fx, "b").await;
    assert_eq!(b.base_sha, commit);
}

/// T5 (D1) — an attached track without a worktree (the pre-#1830 shape) refuses workers with
/// `spawn-failed: refused: track-without-worktree`; the user's checkout is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worktree_less_attached_track_refuses_workers() {
    let w = world().await;
    let fx = &w.fx;
    sqlx::query("UPDATE tracks SET workspace_worktree_path = NULL WHERE id = ?1")
        .bind(fx.track())
        .execute(&fx.pool())
        .await
        .unwrap();
    let status = git(&fx.track_root, &["status", "--porcelain"]);
    let head = git(&fx.track_root, &["rev-parse", "HEAD"]);

    declare_task(fx, "a", json!({})).await;
    let a = wait_task(fx, "a", |task| task.status == TaskStatus::Failed).await;

    let detail = a.status_detail.unwrap_or_default();
    assert!(
        detail.starts_with("spawn-failed: refused: track-without-worktree: "),
        "{detail}"
    );
    assert_eq!(git(&fx.track_root, &["status", "--porcelain"]), status);
    assert_eq!(git(&fx.track_root, &["rev-parse", "HEAD"]), head);
}
