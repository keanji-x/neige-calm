//! #1727 S4 slice 2 PR-B: kernel git delivery for attached workers, wired end to end — the
//! report transaction's delivery row, the keyed forge submission, the scheduler's settlement
//! into `task_candidates` + `task.git_delivery_settled`, the deferred self-report and the
//! `neige_task_ls.candidate` read surface. Design §6 rows A3–A7, A23–A23c, A25–A31.
//!
//! #1893 S6 (the second half of this file): a failed delivery fails its gated task; Track and
//! Area deletion take the delivery tables and candidate refs with them (A9d, A6c).
//!
//! One process per test (nextest): the PATH mutation in `observation_failure_settles_as_commit_failed`
//! relies on that.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::mcp_track_report::{Boot, boot, call_tool, planner_identity, worker_identity};
use crate::task_recovery::{current, declare};
use calm_server::db::sqlite::{
    card_create_with_id_tx, session_set_handle_state_tx, session_start_runtime_tx,
};
use calm_server::decision_sink::CardDecisionSink;
use calm_server::dispatcher::{Dispatcher, TaskFailurePushTestHook};
use calm_server::error::CalmError;
use calm_server::event::{Event, EventBus};
use calm_server::harness::queue::{MutationRefused, QueueMutation};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, Observation, PlannerHarness,
    PlannerHarnessParams, QueueEntryId, recover_harnesses_on_boot,
};
use calm_server::ids::ActorId;
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::model::{CardRole, NewCard, Task, TaskStatus, now_ms};
use calm_server::operation::forge_action_adapter::{FORGE_ACTION_KIND, ForgeActionAdapter};
use calm_server::operation::task_verify_adapter::{TASK_VERIFY_KIND, TaskVerifyAdapter};
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::scheduler::Scheduler;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::test_seams::{KernelWorkspaceLease, take_kernel_workspace_lease_for_test};
use calm_types::git_candidate::{DeliveryFailureCode, DeliverySettlement, DeliveryWakeReason};
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use serde_json::{Value, json};

pub(super) const SETTLED_KIND: &str = "task.git_delivery_settled";
pub(super) const TASK_FAILED_KIND: &str = "task.failed";
pub(super) const GATE_RESULT_KIND: &str = "task.gate_result";
pub(super) const HOOK_EXIT_1: &str = "#!/bin/sh\nexit 1\n";
pub(super) const WAIT: Duration = Duration::from_secs(30);
/// The fixed failure sentences the settlement writes (`prompts/delivery/git-delivery-failures.md`).
pub(super) const FAILURE_SENTENCES: &str =
    include_str!("../../prompts/delivery/git-delivery-failures.md");

pub(super) fn failure_sentence(key: &str) -> String {
    FAILURE_SENTENCES
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find(|(k, _)| *k == key)
        .map(|(_, sentence)| sentence.to_string())
        .unwrap_or_else(|| panic!("no failure sentence for {key}"))
}

// ---------------------------------------------------------------------------
// git helpers (the server's own scrubbed `git`).
// ---------------------------------------------------------------------------

pub(super) fn git_output(dir: &Path, args: &[&str]) -> std::process::Output {
    calm_server::test_seams::neige_git_command_for_test()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
}

pub(super) fn git(dir: &Path, args: &[&str]) -> String {
    let output = git_output(dir, args);
    assert!(
        output.status.success(),
        "git {args:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(super) fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.email", "delivery@example.test"]);
    git(path, &["config", "user.name", "Delivery Test"]);
    std::fs::write(path.join("README.md"), "initial\n").unwrap();
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-q", "-m", "initial"]);
}

pub(super) fn commit_file(dir: &Path, name: &str, content: &str, message: &str) -> String {
    std::fs::write(dir.join(name), content).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// `git --git-dir=<common_dir> rev-parse --verify <ref>^{commit}` — the settlement's own check.
pub(super) fn ref_target(common_dir: &Path, ref_name: &str) -> Option<String> {
    let output = calm_server::test_seams::neige_git_command_for_test()
        .arg(format!("--git-dir={}", common_dir.display()))
        .args([
            "rev-parse",
            "-q",
            "--verify",
            &format!("{ref_name}^{{commit}}"),
        ])
        .output()
        .expect("spawn git");
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// What the Planner does with an attempt's left-over changes before the next worker (#1830 S2 D6:
/// a worker starts only on a clean tree): back to HEAD, untracked files removed.
pub(super) fn undo_worker_changes(checkout: &Path) {
    git(checkout, &["reset", "-q", "--hard", "HEAD"]);
    git(checkout, &["clean", "-fdq"]);
}

pub(super) fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

// ---------------------------------------------------------------------------
// The fixture: the MCP boot, a git repository as the Track workspace, a runtime with the forge
// and task-verify adapters, a Dispatcher whose live listener pushes the Planner harness.
// ---------------------------------------------------------------------------

pub(super) struct Fx {
    pub(super) boot: Boot,
    pub(super) runtime: Arc<OperationRuntime>,
    pub(super) dispatcher: Dispatcher,
    pub(super) harness: HarnessRegistry,
    /// Where `boot.ctx.scheduler_poke` lands: the current Dispatcher's scheduler.
    pub(super) poke_target: Arc<std::sync::RwLock<Arc<Scheduler>>>,
    /// The Track workspace (the main repository, or a linked worktree of it).
    pub(super) track_root: PathBuf,
    /// The track worktree on `neige/track-<id>` (#1830 S2): where every worker runs.
    pub(super) worktree: PathBuf,
    pub(super) workspace_root: PathBuf,
    pub(super) _tmp: tempfile::TempDir,
}

/// The delivery row as the tests read it: identity plus the six settlement columns.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct DeliveryRowView {
    pub(super) delivery_id: String,
    pub(super) ordinal: i64,
    pub(super) operation_key: String,
    pub(super) forge_idempotency_key: String,
    pub(super) settlement: Option<String>,
    pub(super) settled_event_id: Option<i64>,
    pub(super) failure_code: Option<String>,
    pub(super) failure_reason: Option<String>,
    pub(super) retry_allowed: Option<i64>,
    pub(super) wake_reason: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct CandidateRowView {
    pub(super) candidate_id: String,
    pub(super) commit_sha: String,
    pub(super) base_sha: String,
    pub(super) base_is_ancestor: i64,
    pub(super) ref_name: String,
    pub(super) git_common_dir: String,
}

pub(super) async fn fixture() -> Fx {
    fixture_with(|tmp| {
        let repo = tmp.join("repo");
        init_repo(&repo);
        repo
    })
    .await
}

/// `track_root` builds the Track workspace under the temp dir and returns it.
pub(super) async fn fixture_with(track_root: impl FnOnce(&Path) -> PathBuf) -> Fx {
    fixture_on(boot().await, track_root).await
}

/// [`fixture_with`] over an existing [`Boot`] (a file-backed one, for a test that launches the
/// kernel binary against the same database afterwards).
pub(super) async fn fixture_on(boot: Boot, track_root: impl FnOnce(&Path) -> PathBuf) -> Fx {
    fixture_on_with_adapters(boot, track_root, |_| Vec::new()).await
}

/// [`fixture_on`] with further adapters in the runtime (a real worker adapter, for a test that
/// drives a worker operation's `prepare_tx`); `extra` sees the Boot before the runtime exists.
pub(super) async fn fixture_on_with_adapters(
    boot: Boot,
    track_root: impl FnOnce(&Path) -> PathBuf,
    extra: impl FnOnce(&Boot) -> Vec<Arc<dyn ProviderAdapter>>,
) -> Fx {
    fixture_on_with_runtime(boot, track_root, None, |boot, _| extra(boot)).await
}

/// [`fixture_on_with_adapters`] whose runtime also reaches a shared Codex daemon (the one a
/// worker adapter in `extra` spawns on, so the kernel's interrupt of a timed-out or canceled
/// worker reaches it too); `extra` also sees the managed workspace root.
pub(super) async fn fixture_on_with_runtime(
    boot: Boot,
    track_root: impl FnOnce(&Path) -> PathBuf,
    shared_codex: Option<Arc<SharedCodexAppServer>>,
    extra: impl FnOnce(&Boot, &Path) -> Vec<Arc<dyn ProviderAdapter>>,
) -> Fx {
    let tmp = tempfile::Builder::new()
        .prefix("neige-git-delivery-")
        .tempdir()
        .unwrap();
    let track_root = track_root(tmp.path());
    let workspace_root = tmp.path().join("workspaces");
    std::fs::create_dir_all(&workspace_root).unwrap();
    let pool = boot.repo.sqlite_pool().unwrap();
    sqlx::query("UPDATE tracks SET workspace_path = ?1 WHERE id = ?2")
        .bind(track_root.to_str().unwrap())
        .bind(boot.track_id.as_str())
        .execute(&pool)
        .await
        .unwrap();
    let worktree = attach_track_worktree(&pool, boot.track_id.as_str(), &track_root).await;

    let events = boot.ctx.events.clone();
    let operation_repo = Arc::new(SqlxOperationRepo::new(pool.clone()));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let completion = OperationCompletionBus::new();
    let daemon = Arc::new(DaemonClient::new_stub());
    let mut spawn_ctx = SpawnCtx::new(
        route_repo,
        operation_repo.clone(),
        daemon.clone(),
        terminal_renderer.clone(),
        events.clone(),
        completion.clone(),
    );
    if let Some(shared) = shared_codex {
        spawn_ctx = spawn_ctx.with_shared_codex_appserver(shared);
    }
    let gate_logs_dir = boot.ctx.gate_logs_dir.clone();
    let mut adapters = vec![
        Arc::new(ForgeActionAdapter::new()) as Arc<dyn ProviderAdapter>,
        Arc::new(TaskVerifyAdapter::new(gate_logs_dir.clone())) as Arc<dyn ProviderAdapter>,
    ];
    adapters.extend(extra(&boot, &workspace_root));
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        adapters,
        events.clone(),
        completion,
        spawn_ctx,
    ));
    assert!(boot.ctx.operation_runtime.set(runtime.clone()).is_ok());
    let harness = HarnessRegistry::new();
    let dispatcher = spawn_dispatcher(&boot, &runtime, &harness, terminal_renderer, daemon);
    // The tool-side scheduler trigger (a running-task cancel reaps its worker), as
    // `AppState::new` binds it; `respawn_dispatcher` repoints it at the new scheduler.
    let poke_target = Arc::new(std::sync::RwLock::new(dispatcher.scheduler()));
    assert!(
        boot.ctx
            .scheduler_poke
            .set(Arc::new(RepointablePoke(poke_target.clone())))
            .is_ok()
    );
    Fx {
        boot,
        runtime,
        dispatcher,
        harness,
        poke_target,
        track_root,
        worktree,
        workspace_root,
        _tmp: tmp,
    }
}

/// Give the attached Track its track worktree as the create route does (#1830 S1, through the
/// production `ensure_track_worktree`). Returns it.
pub(super) async fn attach_track_worktree(
    pool: &sqlx::SqlitePool,
    track_id: &str,
    checkout: &Path,
) -> PathBuf {
    calm_server::test_seams::attach_track_worktree_for_test(pool, track_id, checkout)
        .await
        .unwrap()
}

/// The tool-side scheduler triggers, forwarded to whichever scheduler `poke_target` names now.
struct RepointablePoke(Arc<std::sync::RwLock<Arc<calm_server::scheduler::Scheduler>>>);

impl calm_server::mcp_server::registry::SchedulerPokes for RepointablePoke {
    fn poke_worker_cleanups(&self) {
        self.0.read().unwrap().poke_worker_cleanups();
    }
}

pub(super) fn spawn_dispatcher(
    boot: &Boot,
    runtime: &Arc<OperationRuntime>,
    harness: &HarnessRegistry,
    terminal_renderer: Arc<TerminalRendererRegistry>,
    daemon: Arc<DaemonClient>,
) -> Dispatcher {
    Dispatcher::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
        boot.repo.clone(),
        boot.ctx.events.clone(),
        boot.ctx.write.clone(),
        Arc::new(CodexClient::new_stub()),
        daemon,
        terminal_renderer,
        None,
        harness.clone(),
        SharedCodexAppServer::new_stub(boot.repo.clone()),
        runtime.clone(),
        4,
        boot.ctx.gate_logs_dir.clone(),
    )
}

impl Fx {
    pub(super) fn pool(&self) -> sqlx::SqlitePool {
        self.boot.repo.sqlite_pool().unwrap()
    }

    pub(super) fn track(&self) -> &str {
        self.boot.track_id.as_str()
    }

    pub(super) fn scheduler(&self) -> Arc<Scheduler> {
        self.dispatcher.scheduler()
    }

    /// A new Dispatcher (live listener + fresh scheduler) over the same runtime, events bus and
    /// harness registry — no recovery, no boot sweep: the live path resumes where
    /// `abort_event_listener_for_test` stopped it.
    pub(super) fn respawn_dispatcher(&mut self) {
        self.dispatcher.abort_event_listener_for_test();
        let route_repo: Arc<dyn calm_server::db::RouteRepo> = self.boot.repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo);
        self.dispatcher = spawn_dispatcher(
            &self.boot,
            &self.runtime,
            &self.harness,
            terminal_renderer,
            Arc::new(DaemonClient::new_stub()),
        );
        *self.poke_target.write().unwrap() = self.dispatcher.scheduler();
    }

    /// A kernel restart: a new Dispatcher, then operation recovery and the boot sweep.
    pub(super) async fn reboot(&mut self) {
        self.respawn_dispatcher();
        let plan = self.runtime.recover_on_boot().await.unwrap();
        self.runtime.apply_recovery(plan).await.unwrap();
        let scheduler = self.scheduler();
        scheduler.mark_context_sweep_boot_complete();
        scheduler.sweep_boot().await;
    }

    /// The `Boot`'s worker card as a Codex worker.
    pub(super) fn codex_worker(&self) -> ToolCallIdentity {
        worker_identity(&self.boot)
    }

    /// The `Boot`'s worker card as a Claude worker (the identity's provider is what the emit
    /// handler reads; `call_tool` bypasses the transport hop that derives it).
    pub(super) fn claude_worker(&self) -> ToolCallIdentity {
        ToolCallIdentity {
            provider: AgentProvider::Claude,
            ..worker_identity(&self.boot)
        }
    }

    /// Another worker card on the same Track with its own live session.
    pub(super) async fn new_worker(&self, name: &str, provider: AgentProvider) -> ToolCallIdentity {
        let card_id = format!("worker-{name}");
        let session_id = format!("{card_id}-session");
        let mut tx = self.pool().begin().await.unwrap();
        card_create_with_id_tx(
            &mut tx,
            card_id.clone(),
            NewCard {
                track_id: self.boot.track_id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            },
            CardRole::Worker,
            true,
            &self.boot.card_role_cache,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        crate::mcp_track_report::seed_non_root_session_with_provider(
            self.boot.repo.as_ref(),
            &self.boot.track_id,
            &calm_server::ids::CardId::from(card_id.clone()),
            &session_id,
            match provider {
                AgentProvider::Claude => calm_types::worker::WorkerProviderKind::Claude,
                _ => calm_types::worker::WorkerProviderKind::Codex,
            },
        )
        .await;
        ToolCallIdentity {
            card_id,
            provider,
            session_id,
            thread_id: format!("{name}-thread"),
            ..worker_identity(&self.boot)
        }
    }

    /// Declare a task and claim its current attempt onto `worker` as `running`, the state a
    /// reporting worker is in. `extra` merges into the declaration (`gate`, `context`, ...).
    pub(super) async fn running_task(
        &self,
        key: &str,
        kind: &str,
        worker: &str,
        extra: Value,
    ) -> Task {
        let mut declaration = json!({
            "key": key, "kind": kind, "goal": format!("deliver {key}"),
            "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true,
            "no_gate_reason": "delivery fixture",
        });
        if let Value::Object(extra) = extra {
            let object = declaration.as_object_mut().unwrap();
            for (k, v) in extra {
                if v.is_null() {
                    object.remove(&k);
                } else {
                    object.insert(k, v);
                }
            }
        }
        declare(&self.boot, declaration).await;
        let task = current(&self.boot, key).await;
        self.claim_running(&task.id, worker).await;
        current(&self.boot, key).await
    }

    pub(super) async fn claim_running(&self, task_id: &str, worker: &str) {
        sqlx::query(
            "UPDATE tasks SET status = 'running', worker_card_id = ?1, updated_at_ms = ?3 WHERE id = ?2",
        )
        .bind(worker)
        .bind(task_id)
        .bind(now_ms())
        .execute(&self.pool())
        .await
        .unwrap();
    }

    /// The production lease sequence for `card` (#1830 S2): prepare the track worktree (refused
    /// when dirty), its HEAD as the base, the kernel-policy row. One held lease per checkout.
    pub(super) async fn kernel_lease(&self, card: &str) -> KernelWorkspaceLease {
        take_kernel_workspace_lease_for_test(&self.pool(), self.track(), card, &self.workspace_root)
            .await
            .unwrap()
    }

    /// Flip the card's held lease to `released` by hand, writing nothing else: a fixture that
    /// stages several attempts at once needs it, as the track's checkout takes one held lease at a
    /// time (#1830 S2).
    pub(super) async fn release_lease_by_hand(&self, card: &str) {
        sqlx::query("UPDATE workspace_leases SET state = 'released' WHERE card_id = ?1")
            .bind(card)
            .execute(&self.pool())
            .await
            .unwrap();
    }

    /// The track's worker branch (#1830 S2 D4).
    pub(super) fn worker_branch(&self) -> String {
        format!("neige/track-{}", self.track())
    }

    pub(super) async fn complete(&self, worker: &ToolCallIdentity, task_id: &str) {
        call_tool(
            &self.boot,
            "neige_task_done",
            worker.clone(),
            json!({"attempt_id": task_id, "result": {"ok": true}}),
        )
        .await
        .unwrap();
    }

    /// The report transaction alone — no forge submission — the state a kernel that died right
    /// after `neige_task_done`'s transaction leaves behind (no crash seam: 5.1.16).
    pub(super) async fn report_only(&self, worker: &ToolCallIdentity, task_id: &str) {
        CardDecisionSink::from_app_context(&self.boot.ctx)
            .commit_worker_task_report(
                worker,
                Event::TaskCompleted {
                    idempotency_key: task_id.to_string(),
                    result: json!({"ok": true}),
                    artifacts: Vec::new(),
                    agent_message: None,
                },
            )
            .await
            .unwrap();
    }

    pub(super) async fn delivery_row(&self, attempt: &str) -> Option<DeliveryRowView> {
        sqlx::query_as(
            "SELECT delivery_id, ordinal, operation_key, forge_idempotency_key, settlement, \
             settled_event_id, failure_code, failure_reason, retry_allowed, wake_reason \
             FROM task_git_deliveries WHERE producer_attempt_id = ?1 ORDER BY ordinal DESC LIMIT 1",
        )
        .bind(attempt)
        .fetch_optional(&self.pool())
        .await
        .unwrap()
    }

    pub(super) async fn delivery_count(&self, attempt: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM task_git_deliveries WHERE producer_attempt_id = ?1",
        )
        .bind(attempt)
        .fetch_one(&self.pool())
        .await
        .unwrap()
    }

    pub(super) async fn candidate_row(&self, attempt: &str) -> Option<CandidateRowView> {
        sqlx::query_as(
            "SELECT candidate_id, commit_sha, base_sha, base_is_ancestor, ref_name, git_common_dir \
             FROM task_candidates WHERE producer_attempt_id = ?1",
        )
        .bind(attempt)
        .fetch_optional(&self.pool())
        .await
        .unwrap()
    }

    pub(super) async fn forge_op_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = ?1")
            .bind(FORGE_ACTION_KIND)
            .fetch_one(&self.pool())
            .await
            .unwrap()
    }

    pub(super) async fn forge_op(
        &self,
        forge_idempotency_key: &str,
    ) -> Option<calm_server::operation::Operation> {
        self.runtime
            .find_by_kind_and_idempotency(FORGE_ACTION_KIND, forge_idempotency_key)
            .await
            .unwrap()
    }

    /// Wait for the attempt's forge Operation to exist and reach a terminal phase.
    pub(super) async fn wait_forge_op(&self, attempt: &str) -> calm_server::operation::Operation {
        let row = self.delivery_row(attempt).await.expect("delivery row");
        let op = tokio::time::timeout(WAIT, async {
            loop {
                if let Some(op) = self.forge_op(&row.forge_idempotency_key).await {
                    break op;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("forge op submitted");
        tokio::time::timeout(WAIT, self.runtime.wait(&op.id))
            .await
            .expect("forge op terminal")
            .unwrap();
        self.forge_op(&row.forge_idempotency_key).await.unwrap()
    }

    /// Every persisted `task.git_delivery_settled` of this Track, oldest first.
    pub(super) async fn settled_events(&self) -> Vec<calm_server::db::TrackEvent> {
        self.boot
            .repo
            .events_for_track(self.track(), &[SETTLED_KIND], None)
            .await
            .unwrap()
    }

    pub(super) async fn settled_events_for(
        &self,
        attempt: &str,
    ) -> Vec<calm_server::db::TrackEvent> {
        self.settled_events()
            .await
            .into_iter()
            .filter(|row| matches!(&row.event, Event::TaskGitDeliverySettled { task_id, .. } if task_id == attempt))
            .collect()
    }

    /// Wait for the attempt's settlement event and return it.
    pub(super) async fn wait_settled(&self, attempt: &str) -> calm_server::db::TrackEvent {
        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(row) = self.settled_events_for(attempt).await.into_iter().next() {
                    break row;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("attempt {attempt} never settled: {:?}", self.debug_state()))
    }

    pub(super) fn debug_state(&self) -> String {
        format!("track {} at {}", self.track(), self.track_root.display())
    }

    pub(super) async fn worktree_committed_events(&self, card: &str) -> Vec<Value> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT payload FROM events WHERE kind = 'worktree.committed' AND scope_card = ?1 ORDER BY id ASC",
        )
        .bind(card)
        .fetch_all(&self.pool())
        .await
        .unwrap();
        rows.into_iter()
            .map(|(payload,)| serde_json::from_str(&payload).unwrap())
            .collect()
    }

    /// `neige_task_ls` (full detail) entry of `key`.
    pub(super) async fn plan_entry(&self, key: &str) -> Value {
        let list = call_tool(
            &self.boot,
            "neige_task_ls",
            planner_identity(&self.boot),
            json!({"detail": "full", "key": key}),
        )
        .await
        .unwrap();
        list["tasks"][0].clone()
    }

    pub(super) async fn plan_summary_entry(&self, key: &str) -> Value {
        let list = call_tool(
            &self.boot,
            "neige_task_ls",
            planner_identity(&self.boot),
            json!({"detail": "summary", "key": key}),
        )
        .await
        .unwrap();
        list["tasks"][0].clone()
    }

    pub(super) async fn task_columns(&self, attempt: &str) -> TaskColumns {
        sqlx::query_as(
            "SELECT status, status_detail, gate_json IS NOT NULL AS gated, gate_pid, \
             gate_pid_starttime, gate_pid_boot_id, finished_at_ms FROM tasks WHERE id = ?1",
        )
        .bind(attempt)
        .fetch_one(&self.pool())
        .await
        .unwrap()
    }

    pub(super) async fn events_for(
        &self,
        kind: &str,
        attempt: &str,
    ) -> Vec<calm_server::db::TrackEvent> {
        self.boot
            .repo
            .events_for_track(self.track(), &[kind], None)
            .await
            .unwrap()
            .into_iter()
            .filter(|row| match &row.event {
                Event::TaskFailed {
                    idempotency_key, ..
                } => idempotency_key == attempt,
                Event::TaskGateResult { task_id, .. } => task_id == attempt,
                Event::TaskGitDeliverySettled { task_id, .. } => task_id == attempt,
                _ => false,
            })
            .collect()
    }

    /// Slice 4: a gate is admitted only after a `candidate` settlement — on a failed delivery no
    /// `#g1` is ever submitted. The admission decision is run by hand
    /// (`drive_gate_for_test`, retried while a live drive still holds `gate:<task>`) and only
    /// then is the absence read: the decision has run, nothing is timed.
    pub(super) async fn assert_no_gate_op(&self, attempt: &str) {
        let task = self
            .boot
            .repo
            .task_get(attempt)
            .await
            .unwrap()
            .expect("task row");
        tokio::time::timeout(WAIT, async {
            loop {
                match self.scheduler().drive_gate_for_test(task.clone()).await {
                    Ok(()) => break,
                    Err(CalmError::Conflict(_)) => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    Err(error) => panic!("gate drive for {attempt}: {error}"),
                }
            }
        })
        .await
        .expect("the live gate drive released gate:<task>");
        assert!(
            self.runtime
                .find_by_kind_and_idempotency(TASK_VERIFY_KIND, &format!("{attempt}#g1"))
                .await
                .unwrap()
                .is_none(),
            "no gate is admitted on a failed delivery (slice 4)"
        );
    }

    /// A task whose worker completed against a pre-commit hook that exits 1: the delivery
    /// settles `failed{commit_failed}`. The hook lives in the repository's common dir, so it
    /// governs every worker of this fixture until `remove_pre_commit`.
    pub(super) async fn hook_failing_task(
        &self,
        key: &str,
        extra: Value,
    ) -> (ToolCallIdentity, Task, KernelWorkspaceLease) {
        let worker = self.codex_worker();
        let lease = self.kernel_lease(&worker.card_id).await;
        install_pre_commit(&lease, HOOK_EXIT_1);
        let task = self
            .running_task(key, "codex", &worker.card_id, extra)
            .await;
        std::fs::write(lease.path.join("worker.txt"), "rejected\n").unwrap();
        self.complete(&worker, &task.id).await;
        (worker, task, lease)
    }

    /// The REST router over this fixture's repository and event bus (its own operation runtime
    /// and harness registry; the forge Operations it fences are read from the shared tables).
    pub(super) async fn http_delete(&self, path: &str) -> (axum::http::StatusCode, String) {
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let areas = calm_server::track_area_cache::TrackAreaCache::new();
        self.boot.repo.seed_track_area_cache(&areas).await.unwrap();
        let write =
            calm_server::state::WriteContext::new(self.boot.card_role_cache.clone(), areas.clone());
        let plugin_host = Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            self.boot.repo.clone(),
            PathBuf::new(),
            self._tmp.path().join("plugins-data"),
            vec![],
            self.boot.ctx.events.clone(),
            write,
        ));
        let state = AppState::from_parts(
            self.boot.repo.clone(),
            self.boot.ctx.events.clone(),
            Arc::new(DaemonClient::new_stub()),
            plugin_host,
            Arc::new(CodexClient::new_stub()),
            Some(self.boot.card_role_cache.clone()),
            Some(areas),
        );
        let app = calm_server::routes::router()
            .layer(axum::middleware::from_fn(
                calm_server::actor::actor_middleware,
            ))
            .with_state(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    pub(super) async fn table_count(&self, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE track_id = ?1"))
            .bind(self.track())
            .fetch_one(&self.pool())
            .await
            .unwrap()
    }

    /// Arm the Dispatcher's `task.failed` hook for `task_id` without holding the handler: the
    /// returned `Notify` fires once the Dispatcher's handler for that envelope has RUN its push
    /// branch (suppressed or delivered) — the barrier `wait_task_failed_handled` waits on.
    pub(super) fn arm_task_failed_barrier(&self, task_id: &str) -> Arc<tokio::sync::Notify> {
        let resume = Arc::new(tokio::sync::Notify::new());
        let finished = Arc::new(tokio::sync::Notify::new());
        self.dispatcher
            .set_task_failure_push_hook_for_test(TaskFailurePushTestHook {
                task_id: task_id.to_string(),
                entered: Arc::new(tokio::sync::Notify::new()),
                resume: resume.clone(),
                finished: finished.clone(),
            });
        // A stored permit: the handler passes its `resume` gate without waiting.
        resume.notify_one();
        finished
    }

    /// A live Planner harness for this Track, registered where the Dispatcher pushes.
    pub(super) async fn planner(&self) -> PlannerHarness {
        let worker_session_id = planner_identity(&self.boot).session_id;
        let areas = calm_server::track_area_cache::TrackAreaCache::new();
        self.boot.repo.seed_track_area_cache(&areas).await.unwrap();
        let mut snapshot = HarnessSnapshot::initial(0, vec![]);
        snapshot.phase = HarnessPhaseTag::Idle;
        snapshot.last_thread_id = Some("planner-observer".into());
        let mut tx = self.pool().begin().await.unwrap();
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: worker_session_id.clone(),
                card_id: self.boot.planner_card_id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: Some("planner-observer".into()),
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let handle = PlannerHarness::run(PlannerHarnessParams {
            worker_session_id: worker_session_id.clone(),
            track_id: self.boot.track_id.clone(),
            card_id: self.boot.planner_card_id.clone(),
            thread_id: Some("planner-observer".into()),
            repo: self.boot.repo.clone(),
            events: self.boot.ctx.events.clone(),
            card_role_cache: self.boot.card_role_cache.clone(),
            track_area_cache: areas,
            backend: SharedCodexAppServer::new_stub(self.boot.repo.clone()).into(),
            live_replies: calm_server::harness::LiveReplies::for_test(),
            config: HarnessConfig::default(),
            snapshot,
        });
        handle
            .force_phase_for_dev(HarnessPhaseTag::TurnRunning)
            .await
            .unwrap();
        self.harness.insert(worker_session_id, handle.clone());
        handle
    }
}

pub(super) async fn observations(handle: &PlannerHarness) -> Vec<Observation> {
    handle.snapshot().await.pending_observations().to_vec()
}

/// Wait until the harness holds `n` pending observations (or fail with what it holds).
pub(super) async fn wait_observations(handle: &PlannerHarness, n: usize) -> Vec<Observation> {
    tokio::time::timeout(WAIT, async {
        loop {
            let pending = observations(handle).await;
            if pending.len() >= n {
                break pending;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("harness never reached {n} observations"))
}

/// Settle for a moment and assert the harness holds exactly `expected` observations. A timing
/// check: use it only where no envelope was emitted at all (nothing to barrier on); where an
/// envelope was emitted, prove it handled (`wait_task_failed_handled`, `wait_observations`) and
/// read the count through `assert_observations_exactly`.
pub(super) async fn assert_observations_settle_at(
    handle: &PlannerHarness,
    expected: usize,
) -> Vec<Observation> {
    tokio::time::sleep(Duration::from_millis(400)).await;
    let pending = observations(handle).await;
    assert_eq!(pending.len(), expected, "{pending:?}");
    pending
}

/// The Dispatcher's handler for the armed `task.failed` has run (`Fx::arm_task_failed_barrier`).
pub(super) async fn wait_task_failed_handled(finished: &tokio::sync::Notify) {
    tokio::time::timeout(WAIT, finished.notified())
        .await
        .expect("the Dispatcher handled the task.failed envelope");
}

/// Drain the harness ingress, then assert exactly `expected` observations. The refused no-op
/// queue command rides the same FIFO as observation deliveries, so its answer proves everything
/// the Dispatcher enqueued before it has been applied (PR-A's
/// `deferred_settlement_is_silent_live_and_on_replay` technique).
pub(super) async fn assert_observations_exactly(
    handle: &PlannerHarness,
    expected: usize,
) -> Vec<Observation> {
    assert_eq!(
        handle
            .mutate_pending_entry(
                QueueMutation::Delete {
                    entry_id: QueueEntryId::from_wire("absent-observation-barrier".into()),
                    if_entry_rev: 1,
                },
                ActorId::User,
            )
            .await
            .unwrap(),
        Err(MutationRefused::NotFound)
    );
    let pending = observations(handle).await;
    assert_eq!(pending.len(), expected, "{pending:?}");
    pending
}

pub(super) fn settled_result(
    row: &calm_server::db::TrackEvent,
) -> (&DeliverySettlement, DeliveryWakeReason) {
    match &row.event {
        Event::TaskGitDeliverySettled {
            result,
            wake_reason,
            ..
        } => (result, *wake_reason),
        other => panic!("not a settlement: {other:?}"),
    }
}

pub(super) fn candidate_of(result: &DeliverySettlement) -> (&str, &str, &str, bool) {
    match result {
        DeliverySettlement::Candidate {
            candidate_id,
            commit_sha,
            base_sha,
            base_is_ancestor,
        } => (candidate_id, commit_sha, base_sha, *base_is_ancestor),
        other => panic!("not a candidate: {other:?}"),
    }
}

pub(super) fn failure_of(result: &DeliverySettlement) -> (DeliveryFailureCode, &str, bool) {
    match result {
        DeliverySettlement::Failed {
            code,
            reason,
            retry_allowed,
        } => (*code, reason, *retry_allowed),
        other => panic!("not a failure: {other:?}"),
    }
}

/// `<result_path>.code` of the attempt's forge action, as the wrapper left it.
pub(super) async fn result_code(fx: &Fx, attempt: &str) -> Option<i32> {
    let op = fx.wait_forge_op(attempt).await;
    let result_path = PathBuf::from(op.payload["result_path"].as_str().unwrap());
    let mut code_path = result_path.into_os_string();
    code_path.push(".code");
    std::fs::read_to_string(code_path)
        .ok()
        .map(|text| text.trim().parse().unwrap())
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(super) struct TaskColumns {
    pub(super) status: TaskStatus,
    pub(super) status_detail: Option<String>,
    pub(super) gated: bool,
    pub(super) gate_pid: Option<i64>,
    pub(super) gate_pid_starttime: Option<i64>,
    pub(super) gate_pid_boot_id: Option<String>,
    pub(super) finished_at_ms: Option<i64>,
}

pub(super) fn install_pre_commit(lease: &KernelWorkspaceLease, body: &str) {
    let hooks = lease.git_common_dir.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    write_executable(&hooks.join("pre-commit"), body);
}

pub(super) fn remove_pre_commit(lease: &KernelWorkspaceLease) {
    std::fs::remove_file(lease.git_common_dir.join("hooks").join("pre-commit")).unwrap();
}

/// A pre-commit hook that blocks until `flag` exists, then exits `code`.
pub(super) fn hook_waiting_for(flag: &Path, code: i32) -> String {
    format!(
        "#!/bin/sh\nuntil [ -f '{}' ]; do sleep 0.1; done\nexit {code}\n",
        flag.display()
    )
}

/// `git --git-dir=<common dir> for-each-ref refs/neige/candidates/<track>/`.
pub(super) fn candidate_refs(common_dir: &Path, track_id: &str) -> Vec<String> {
    let output = calm_server::test_seams::neige_git_command_for_test()
        .arg(format!("--git-dir={}", common_dir.display()))
        .args([
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/neige/candidates/{track_id}/"),
        ])
        .output()
        .expect("spawn git");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// A3 / A3b / A7: a Claude worker's completion becomes a kernel candidate.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_worker_completion_yields_kernel_candidate() {
    let mut fx = fixture().await;
    // The report handler's own submission is asserted before any scheduler pass could submit the
    // row under the same key (a pass would hide a handler that skips Claude). Every pass in this
    // fixture starts from the Dispatcher's live listener (`plan.updated` from the declaration,
    // `task.completed` from the report; the backstop sweeps are boot-gated), and `poke` counts
    // before it spawns an unjoined pass, so the listener is stopped before the test publishes its
    // first envelope: no handler is ever spawned, nothing pokes, and `claim_running` reaches
    // `running` by SQL. The live path resumes after the window.
    fx.dispatcher.abort_event_listener_for_test();
    let scheduler = fx.scheduler();
    let planner = fx.planner().await;
    let worker = fx.claude_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("deliver", "claude", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "delivered\n").unwrap();

    fx.complete(&worker, &task.id).await;
    let row = fx.delivery_row(&task.id).await.expect("delivery row");
    assert_eq!(row.ordinal, 1);
    assert!(
        row.settlement.is_none(),
        "no scheduler pass has run: {row:?}"
    );
    let op = fx
        .forge_op(&row.forge_idempotency_key)
        .await
        .expect("the report handler submitted the delivery before returning");
    assert_eq!(op.operation_key, row.operation_key, "under the row's key");
    assert_eq!(fx.forge_op_count().await, 1);
    assert_eq!(
        scheduler.poke_count_for_test(),
        0,
        "the stopped listener never poked the scheduler"
    );

    // The live path resumes: a fresh listener pushes the settlement to the Planner, and the
    // scheduler drives the row (Operation present) to settlement.
    fx.respawn_dispatcher();
    fx.scheduler().poke(fx.boot.track_id.clone());
    let settled = fx.wait_settled(&task.id).await;

    let row = fx.delivery_row(&task.id).await.expect("delivery row");
    assert_eq!(row.ordinal, 1);
    assert_eq!(row.settlement.as_deref(), Some("candidate"));
    let (result, wake_reason) = settled_result(&settled);
    let (candidate_id, commit_sha, base_sha, base_is_ancestor) = candidate_of(result);
    assert_eq!(candidate_id, row.delivery_id);
    assert_eq!(base_sha, lease.base_sha);
    assert_ne!(commit_sha, base_sha, "the worker's edit was committed");
    assert!(base_is_ancestor);
    assert_eq!(wake_reason, DeliveryWakeReason::UngatedCandidate);

    // `worktree.committed{delivery_id, base_is_ancestor}` from the script's JSON line.
    let committed = fx.worktree_committed_events(&worker.card_id).await;
    assert_eq!(committed.len(), 1, "{committed:?}");
    assert_eq!(committed[0]["delivery_id"], row.delivery_id);
    assert_eq!(committed[0]["base_is_ancestor"], true);
    assert_eq!(committed[0]["commit_sha"], commit_sha);
    assert_eq!(committed[0]["branch"], fx.worker_branch());

    // One candidate row; the ref in the lease's common dir points at its commit.
    let candidate = fx.candidate_row(&task.id).await.expect("candidate row");
    assert_eq!(candidate.candidate_id, row.delivery_id);
    assert_eq!(candidate.commit_sha, commit_sha);
    assert_eq!(candidate.base_is_ancestor, 1);
    assert_eq!(
        candidate.ref_name,
        format!(
            "refs/neige/candidates/{}/{}/{}",
            fx.track(),
            worker.card_id,
            row.delivery_id
        )
    );
    assert_eq!(
        ref_target(&lease.git_common_dir, &candidate.ref_name).as_deref(),
        Some(commit_sha)
    );
    assert_eq!(git(&lease.path, &["show", "HEAD:worker.txt"]), "delivered");

    // Read surface and exactly one settlement event, one Planner turn.
    let entry = fx.plan_entry("deliver").await;
    assert_eq!(entry["candidate"]["binding"], "bound", "{entry}");
    assert_eq!(entry["candidate"]["delivery"]["state"], "committed");
    assert_eq!(entry["candidate"]["delivery"]["commit_sha"], commit_sha);
    assert_eq!(
        entry["candidate"]["delivery"]["candidate_id"],
        row.delivery_id
    );
    assert_eq!(entry["candidate"]["base_sha"], lease.base_sha);
    let summary = fx.plan_summary_entry("deliver").await;
    assert_eq!(summary["candidate"]["binding"], "bound", "{summary}");
    assert_eq!(summary["candidate"]["delivery"]["state"], "committed");
    assert_eq!(summary["candidate"]["delivery"]["commit_sha"], commit_sha);
    assert_eq!(fx.settled_events_for(&task.id).await.len(), 1);
    assert_eq!(fx.forge_op_count().await, 1);
    let pending = wait_observations(&planner, 1).await;
    assert!(
        matches!(
            &pending[0],
            Observation::TaskGitDeliverySettled {
                result: DeliverySettlement::Candidate { .. },
                ..
            }
        ),
        "{pending:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unchanged_worker_completion_settles_no_change() {
    let fx = fixture().await;
    let worker = fx.claude_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("unchanged", "claude", &worker.card_id, json!({}))
        .await;

    fx.complete(&worker, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;

    let (result, _) = settled_result(&settled);
    let (_, commit_sha, base_sha, base_is_ancestor) = candidate_of(result);
    assert_eq!(commit_sha, lease.base_sha);
    assert_eq!(commit_sha, base_sha);
    assert!(base_is_ancestor);
    let candidate = fx.candidate_row(&task.id).await.expect("candidate row");
    assert_eq!(candidate.commit_sha, candidate.base_sha);
    assert_eq!(
        ref_target(&lease.git_common_dir, &candidate.ref_name).as_deref(),
        Some(lease.base_sha.as_str())
    );
    let entry = fx.plan_entry("unchanged").await;
    assert_eq!(
        entry["candidate"]["delivery"]["state"], "no_change",
        "{entry}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_completion_has_one_delivery() {
    let fx = fixture().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("twice", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "once\n").unwrap();

    fx.complete(&worker, &task.id).await;
    // The repeated report is admitted as an idempotent success and writes nothing.
    fx.complete(&worker, &task.id).await;
    fx.wait_settled(&task.id).await;
    fx.complete(&worker, &task.id).await;

    assert_eq!(fx.delivery_count(&task.id).await, 1);
    assert_eq!(fx.forge_op_count().await, 1);
    assert_eq!(fx.settled_events_for(&task.id).await.len(), 1);
    let candidates: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM task_candidates WHERE producer_attempt_id = ?1")
            .bind(&task.id)
            .fetch_one(&fx.pool())
            .await
            .unwrap();
    assert_eq!(candidates, 1);
}

// ---------------------------------------------------------------------------
// The candidate comes from the Operation result, never from events.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_row_is_minted_from_operation_result_not_events() {
    let fx = fixture().await;
    // No live listener: the report's own submission runs the script, nothing settles until the
    // test schedules the Track itself.
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("from-result", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "result\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let op = fx.wait_forge_op(&task.id).await;
    assert!(
        matches!(op.phase, calm_server::operation::Phase::Succeeded),
        "{:?}",
        op.phase
    );
    let op_commit = match fx
        .runtime
        .operation_result(&op.id)
        .await
        .unwrap()
        .unwrap()
        .outcome
    {
        calm_server::operation::OperationOutcome::Succeeded { result } => {
            result["event"]["commit_sha"].as_str().unwrap().to_string()
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(fx.worktree_committed_events(&worker.card_id).await.len(), 1);
    assert!(
        fx.candidate_row(&task.id).await.is_none(),
        "not settled yet"
    );

    // The event is gone before the settlement runs.
    sqlx::query("DELETE FROM events WHERE kind = 'worktree.committed' AND scope_card = ?1")
        .bind(&worker.card_id)
        .execute(&fx.pool())
        .await
        .unwrap();
    assert!(
        fx.worktree_committed_events(&worker.card_id)
            .await
            .is_empty()
    );

    fx.scheduler()
        .schedule_track(fx.boot.track_id.clone())
        .await;
    let settled = fx.wait_settled(&task.id).await;
    let (result, _) = settled_result(&settled);
    let (_, commit_sha, _, _) = candidate_of(result);
    let candidate = fx
        .candidate_row(&task.id)
        .await
        .expect("candidate minted from the result");
    assert_eq!(candidate.commit_sha, op_commit);
    assert_eq!(candidate.commit_sha, commit_sha);
    assert_eq!(
        ref_target(&lease.git_common_dir, &candidate.ref_name).as_deref(),
        Some(op_commit.as_str())
    );
}

// ---------------------------------------------------------------------------
// A4 / A4b: the crash window with no result file — the ref is the truth, not HEAD.
// ---------------------------------------------------------------------------

/// A parked forge Operation with dead-pid artifacts and no `.code`/`.stdout`: the live
/// completion is rewound to the state a kernel death after the script (but before the wrapper
/// wrote its files) leaves behind.
pub(super) async fn park_without_result_file(fx: &Fx, attempt: &str) -> PathBuf {
    let op = fx.wait_forge_op(attempt).await;
    let result_path = PathBuf::from(op.payload["result_path"].as_str().unwrap());
    for suffix in ["", ".code", ".stdout"] {
        let mut path = result_path.clone().into_os_string();
        path.push(suffix);
        let _ = std::fs::remove_file(path);
    }
    let artifacts = json!({"pid": 1, "pgid": 1, "start_time": 1, "boot_id": "boot-dead", "log_path": null, "extra": {}});
    sqlx::query(
        "UPDATE operations SET phase = 'parked', parked_at_ms = ?1, parked_deadline_ms = ?2, \
         spawn_artifacts_json = ?3, phase_detail_json = NULL, last_error = NULL WHERE id = ?4",
    )
    .bind(now_ms())
    .bind(now_ms() + 600_000)
    .bind(artifacts.to_string())
    .bind(&op.id)
    .execute(&fx.pool())
    .await
    .unwrap();
    result_path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_window_without_result_file_reads_ref_not_head() {
    let mut fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("crash-ref", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "c1\n").unwrap();
    fx.complete(&worker, &task.id).await;
    park_without_result_file(&fx, &task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    let ref_name = format!(
        "refs/neige/candidates/{}/{}/{}",
        fx.track(),
        worker.card_id,
        row.delivery_id
    );
    let c1 = ref_target(&lease.git_common_dir, &ref_name).expect("the script pinned C1");
    // A clean foreign commit on the branch after the crash: HEAD moves, the ref does not.
    let c2 = commit_file(&lease.path, "later.txt", "c2\n", "c2");
    assert_ne!(c1, c2);
    assert!(fx.candidate_row(&task.id).await.is_none());

    fx.reboot().await;
    let settled = fx.wait_settled(&task.id).await;

    let (result, _) = settled_result(&settled);
    let (_, commit_sha, _, base_is_ancestor) = candidate_of(result);
    assert_eq!(commit_sha, c1, "the candidate is the ref target, not HEAD");
    assert!(base_is_ancestor);
    let candidate = fx.candidate_row(&task.id).await.unwrap();
    assert_eq!(candidate.commit_sha, c1);
    assert_eq!(git(&lease.path, &["rev-parse", "HEAD"]), c2);
}

pub(super) async fn crashed_before_ref(
    fx: &mut Fx,
) -> (ToolCallIdentity, Task, KernelWorkspaceLease) {
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("crash-noref", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "c1\n").unwrap();
    fx.complete(&worker, &task.id).await;
    park_without_result_file(fx, &task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    let ref_name = format!(
        "refs/neige/candidates/{}/{}/{}",
        fx.track(),
        worker.card_id,
        row.delivery_id
    );
    // Died before `update-ref`: the ref never existed.
    git(&lease.path, &["update-ref", "-d", &ref_name]);
    assert_eq!(ref_target(&lease.git_common_dir, &ref_name), None);
    (worker, task, lease)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ref_absent_after_crash_is_failed_not_candidate() {
    let mut fx = fixture().await;
    let (_, task, _) = crashed_before_ref(&mut fx).await;
    let planner = fx.planner().await;

    fx.reboot().await;
    let settled = fx.wait_settled(&task.id).await;

    let (result, wake_reason) = settled_result(&settled);
    let (code, _, retry_allowed) = failure_of(result);
    assert_eq!(code, DeliveryFailureCode::CommitFailed);
    assert!(retry_allowed);
    assert_eq!(wake_reason, DeliveryWakeReason::Failed);
    assert!(fx.candidate_row(&task.id).await.is_none(), "no candidate");
    let pending = wait_observations(&planner, 1).await;
    assert!(
        matches!(
            &pending[0],
            Observation::TaskGitDeliverySettled {
                result: DeliverySettlement::Failed {
                    code: DeliveryFailureCode::CommitFailed,
                    ..
                },
                ..
            }
        ),
        "{pending:?}"
    );
    assert_observations_settle_at(&planner, 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_settlement_row_carries_code_reason_retry() {
    let mut fx = fixture().await;
    let (_, task, _) = crashed_before_ref(&mut fx).await;

    fx.reboot().await;
    let settled = fx.wait_settled(&task.id).await;

    // The settlement transaction committed (the event exists) and the three columns are set.
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(row.settlement.as_deref(), Some("failed"));
    assert_eq!(row.settled_event_id, Some(settled.id));
    assert_eq!(row.failure_code.as_deref(), Some("commit_failed"));
    assert_eq!(
        row.failure_reason.as_deref(),
        Some(failure_sentence("no_result").as_str())
    );
    assert_eq!(row.retry_allowed, Some(1));
    assert_eq!(row.wake_reason.as_deref(), Some("failed"));
    let (result, _) = settled_result(&settled);
    let (_, reason, _) = failure_of(result);
    assert_eq!(reason, row.failure_reason.as_deref().unwrap());
    let entry = fx.plan_entry("crash-noref").await;
    assert_eq!(entry["candidate"]["delivery"]["state"], "failed", "{entry}");
    assert_eq!(
        entry["candidate"]["delivery"]["failure"]["code"],
        "commit_failed"
    );
    // The row keeps `retry_allowed` (0113); the Planner read surface does not show it (#1893 S6).
    assert!(
        entry["candidate"]["delivery"]["failure"]
            .get("retry_allowed")
            .is_none(),
        "{entry}"
    );
    let summary = fx.plan_summary_entry("crash-noref").await;
    assert_eq!(
        summary["candidate"]["delivery"]["failure"]["code"], "commit_failed",
        "{summary}"
    );
}

// ---------------------------------------------------------------------------
// A5: a failed delivery settles once, wakes the Planner once, and a second pass is a no-op.
// ---------------------------------------------------------------------------

pub(super) async fn failing_hook_delivery(fx: &Fx) -> (ToolCallIdentity, Task) {
    let (worker, task, _) = fx.hook_failing_task("hook-red", json!({})).await;
    (worker, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_delivery_emits_exactly_one_settled_event_and_wakes_planner() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let (_, task) = failing_hook_delivery(&fx).await;
    let settled = fx.wait_settled(&task.id).await;

    let (result, wake_reason) = settled_result(&settled);
    let (code, reason, retry_allowed) = failure_of(result);
    assert_eq!(code, DeliveryFailureCode::CommitFailed);
    assert_eq!(reason, failure_sentence("git").replace("{code}", "1"));
    assert!(retry_allowed);
    assert_eq!(wake_reason, DeliveryWakeReason::Failed);
    assert_eq!(fx.settled_events_for(&task.id).await.len(), 1);
    assert_eq!(
        current(&fx.boot, "hook-red").await.status,
        TaskStatus::Done,
        "the task row does not move"
    );
    let pending = wait_observations(&planner, 1).await;
    assert!(
        matches!(
            &pending[0],
            Observation::TaskGitDeliverySettled {
                result: DeliverySettlement::Failed { .. },
                ..
            }
        ),
        "{pending:?}"
    );
    assert_observations_settle_at(&planner, 1).await;
    assert_eq!(result_code(&fx, &task.id).await, Some(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settled_delivery_second_pass_is_noop() {
    let fx = fixture().await;
    let (_, task) = failing_hook_delivery(&fx).await;
    let settled = fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();

    // The second executor: the settlement is called again directly and through a whole pass.
    fx.scheduler()
        .settle_git_delivery_for_test(&row.delivery_id)
        .await
        .expect("a settled row is a no-op, not a trigger error");
    fx.scheduler()
        .schedule_track(fx.boot.track_id.clone())
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let events = fx.settled_events_for(&task.id).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].id, settled.id);
    assert_eq!(fx.delivery_row(&task.id).await.unwrap(), row);
}

impl PartialEq for DeliveryRowView {
    fn eq(&self, other: &Self) -> bool {
        format!("{self:?}") == format!("{other:?}")
    }
}

// ---------------------------------------------------------------------------
// A6 / A6b: the persistent hand-off survives a crash before the submission.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delivery_row_survives_crash_before_submission() {
    let mut fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("handoff", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "handoff\n").unwrap();

    fx.report_only(&worker, &task.id).await;
    let row = fx
        .delivery_row(&task.id)
        .await
        .expect("the report tx inserted the row");
    assert!(row.settlement.is_none());
    assert_eq!(fx.forge_op_count().await, 0, "no submission happened");
    assert_eq!(current(&fx.boot, "handoff").await.status, TaskStatus::Done);

    fx.reboot().await;
    fx.wait_settled(&task.id).await;
    let op = fx
        .forge_op(&row.forge_idempotency_key)
        .await
        .expect("the boot sweep submitted it");
    assert_eq!(op.operation_key, row.operation_key);
    assert_eq!(fx.forge_op_count().await, 1);
    let candidate = fx.candidate_row(&task.id).await.expect("candidate");
    assert_eq!(
        ref_target(&lease.git_common_dir, &candidate.ref_name).as_deref(),
        Some(candidate.commit_sha.as_str())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resubmitted_delivery_reuses_persisted_operation_key() {
    let mut fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("keyed", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "keyed\n").unwrap();
    fx.report_only(&worker, &task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();

    fx.reboot().await;
    fx.wait_settled(&task.id).await;

    let (key, idem): (String, String) =
        sqlx::query_as("SELECT operation_key, idempotency_key FROM operations WHERE kind = ?1")
            .bind(FORGE_ACTION_KIND)
            .fetch_one(&fx.pool())
            .await
            .unwrap();
    assert_eq!(
        key, row.operation_key,
        "the row's key is the Operation's key"
    );
    assert_eq!(idem, row.forge_idempotency_key);
    // The report handler's own submission, arriving later, dedups against the same key.
    fx.complete(&worker, &task.id).await;
    assert_eq!(fx.forge_op_count().await, 1);
    assert_eq!(fx.settled_events_for(&task.id).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ungated_delivery_crashed_before_submission_settles_on_boot() {
    let mut fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("boot-settle", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "boot\n").unwrap();
    fx.report_only(&worker, &task.id).await;
    assert_eq!(
        current(&fx.boot, "boot-settle").await.status,
        TaskStatus::Done
    );
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(fx.forge_op_count().await, 0);
    let planner = fx.planner().await;

    // Boot with no Planner action, no pending task, no verifying row: only the delivery sweep
    // can bring `schedule_pass` to this Track.
    fx.reboot().await;
    let settled = fx.wait_settled(&task.id).await;

    let op = fx.forge_op(&row.forge_idempotency_key).await.expect("op");
    assert_eq!(op.operation_key, row.operation_key);
    assert!(fx.candidate_row(&task.id).await.is_some());
    assert_eq!(fx.settled_events_for(&task.id).await.len(), 1);
    let (result, wake_reason) = settled_result(&settled);
    candidate_of(result);
    assert_eq!(wake_reason, DeliveryWakeReason::UngatedCandidate);
    let pending = wait_observations(&planner, 1).await;
    assert!(
        matches!(
            &pending[0],
            Observation::TaskGitDeliverySettled {
                result: DeliverySettlement::Candidate { .. },
                ..
            }
        ),
        "{pending:?}"
    );
    assert_observations_settle_at(&planner, 1).await;
}

// ---------------------------------------------------------------------------
// A6c / A27: the Track workspace is a linked worktree; the settlement reads the common dir.
// ---------------------------------------------------------------------------

/// A main repository and a linked worktree of it as the Track workspace.
pub(super) fn linked_worktree_track(tmp: &Path) -> PathBuf {
    let main = tmp.join("main");
    init_repo(&main);
    let track = tmp.join("track-wt");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "track",
            track.to_str().unwrap(),
            "HEAD",
        ],
    );
    track
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settlement_uses_lease_common_dir_when_track_cwd_moved() {
    let fx = fixture_with(linked_worktree_track).await;
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    assert!(lease.path.starts_with(&fx.track_root));
    let main_git = fx
        .track_root
        .parent()
        .unwrap()
        .join("main")
        .join(".git")
        .canonicalize()
        .unwrap();
    assert_eq!(lease.git_common_dir, main_git);
    let task = fx
        .running_task("moved-cwd", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "moved\n").unwrap();
    fx.complete(&worker, &task.id).await;
    fx.wait_forge_op(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    let ref_name = format!(
        "refs/neige/candidates/{}/{}/{}",
        fx.track(),
        worker.card_id,
        row.delivery_id
    );
    let pinned = ref_target(&lease.git_common_dir, &ref_name).expect("ref");

    // The script ran; before the settlement the Track cwd (and the lease under it) moves away.
    let moved = fx.track_root.with_file_name("track-wt-moved");
    std::fs::rename(&fx.track_root, &moved).unwrap();
    assert!(!fx.track_root.exists());

    fx.scheduler()
        .schedule_track(fx.boot.track_id.clone())
        .await;
    let settled = fx.wait_settled(&task.id).await;
    let (result, _) = settled_result(&settled);
    let (_, commit_sha, _, _) = candidate_of(result);
    assert_eq!(commit_sha, pinned);
    let candidate = fx
        .candidate_row(&task.id)
        .await
        .expect("settled as a candidate, not unresolved");
    assert_eq!(candidate.commit_sha, pinned);
    assert_eq!(PathBuf::from(&candidate.git_common_dir), main_git);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn linked_worktree_track_delivers() {
    let fx = fixture_with(linked_worktree_track).await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let main_git = fx
        .track_root
        .parent()
        .unwrap()
        .join("main")
        .join(".git")
        .canonicalize()
        .unwrap();
    let task = fx
        .running_task("linked", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "linked\n").unwrap();

    fx.complete(&worker, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;

    let (result, _) = settled_result(&settled);
    let (_, commit_sha, _, _) = candidate_of(result);
    let candidate = fx.candidate_row(&task.id).await.expect("candidate");
    assert_eq!(candidate.commit_sha, commit_sha);
    assert_eq!(
        PathBuf::from(&candidate.git_common_dir),
        main_git,
        "the main repository's .git"
    );
    assert_eq!(
        ref_target(&main_git, &candidate.ref_name).as_deref(),
        Some(commit_sha)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moved_worktree_behind_symlink_is_provenance_mismatch() {
    let fx = fixture().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("symlinked", "codex", &worker.card_id, json!({}))
        .await;
    // The lease worktree is moved elsewhere and a symlink left at its path.
    let elsewhere = fx.track_root.parent().unwrap().join("elsewhere");
    git(
        &fx.track_root,
        &[
            "worktree",
            "move",
            lease.path.to_str().unwrap(),
            elsewhere.to_str().unwrap(),
        ],
    );
    std::os::unix::fs::symlink(&elsewhere, &lease.path).unwrap();
    std::fs::write(elsewhere.join("worker.txt"), "moved\n").unwrap();

    fx.complete(&worker, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;

    let (result, _) = settled_result(&settled);
    let (code, reason, retry_allowed) = failure_of(result);
    assert_eq!(code, DeliveryFailureCode::ProvenanceMismatch);
    assert!(reason.starts_with(&failure_sentence("10")), "{reason}");
    assert!(
        reason.contains("provenance realpath="),
        "the observation line is the evidence: {reason}"
    );
    assert!(retry_allowed);
    assert_eq!(result_code(&fx, &task.id).await, Some(10));
    assert!(fx.candidate_row(&task.id).await.is_none());
    assert_eq!(
        git(&elsewhere, &["status", "--porcelain"]),
        "?? worker.txt",
        "nothing staged"
    );
}

// ---------------------------------------------------------------------------
// A25 / A25b: the script refuses a switched branch and an in-progress operation.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delivery_refuses_switched_branch_as_provenance_mismatch() {
    let fx = fixture().await;
    let cases: [(&str, &[&str]); 2] = [
        ("other-branch", &["checkout", "-q", "-b", "other"]),
        ("detached", &["checkout", "-q", "--detach"]),
    ];
    for (name, checkout) in cases {
        let worker = fx.new_worker(name, AgentProvider::Codex).await;
        let lease = fx.kernel_lease(&worker.card_id).await;
        let task = fx
            .running_task(name, "codex", &worker.card_id, json!({}))
            .await;
        git(&lease.path, checkout);
        std::fs::write(lease.path.join("worker.txt"), "switched\n").unwrap();
        let head_before = git(&lease.path, &["rev-parse", "HEAD"]);

        fx.complete(&worker, &task.id).await;
        let settled = fx.wait_settled(&task.id).await;

        let (result, _) = settled_result(&settled);
        let (code, reason, _) = failure_of(result);
        assert_eq!(code, DeliveryFailureCode::ProvenanceMismatch, "{name}");
        assert_eq!(
            reason,
            failure_sentence("11"),
            "{name}: the branch sentence, no evidence line"
        );
        assert_eq!(result_code(&fx, &task.id).await, Some(11), "{name}");
        assert_eq!(
            git(&lease.path, &["rev-parse", "HEAD"]),
            head_before,
            "{name}: no commit"
        );
        let row = fx.delivery_row(&task.id).await.unwrap();
        let ref_name = format!(
            "refs/neige/candidates/{}/{}/{}",
            fx.track(),
            worker.card_id,
            row.delivery_id
        );
        assert_eq!(
            ref_target(&lease.git_common_dir, &ref_name),
            None,
            "{name}: no ref"
        );
        assert!(fx.candidate_row(&task.id).await.is_none(), "{name}");
        // The Planner puts the checkout back for the next worker (#1830 S2 D6).
        std::fs::remove_file(lease.path.join("worker.txt")).unwrap();
        git(&lease.path, &["checkout", "-q", &fx.worker_branch()]);
    }

    // The positive case: the track branch delivers.
    let worker = fx.new_worker("on-branch", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("on-branch", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "on branch\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;
    candidate_of(settled_result(&settled).0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delivery_refuses_in_progress_merge() {
    enum Op {
        ConflictMerge,
        CleanMerge,
        ConflictCherryPick,
        ResolvedCherryPick,
    }
    // (name, operation, pseudo-ref left behind, both sides edit README, evidence names README)
    let cases = [
        (
            "conflict-merge",
            Op::ConflictMerge,
            "MERGE_HEAD",
            true,
            true,
        ),
        ("clean-merge", Op::CleanMerge, "MERGE_HEAD", false, false),
        (
            "conflict-pick",
            Op::ConflictCherryPick,
            "CHERRY_PICK_HEAD",
            true,
            true,
        ),
        (
            "resolved-pick",
            Op::ResolvedCherryPick,
            "CHERRY_PICK_HEAD",
            true,
            false,
        ),
    ];
    for (name, op, pseudo_ref, conflicting, unmerged_evidence) in cases {
        let fx = fixture().await;
        let worker = fx.codex_worker();
        let lease = fx.kernel_lease(&worker.card_id).await;
        let task = fx
            .running_task(name, "codex", &worker.card_id, json!({}))
            .await;
        let slice = fx.worker_branch();
        // `other`: one commit off the base; the slice branch: one commit of its own.
        git(&lease.path, &["checkout", "-q", "-b", "other"]);
        if conflicting {
            commit_file(&lease.path, "README.md", "other\n", "other");
        } else {
            commit_file(&lease.path, "other.txt", "other\n", "other");
        }
        git(&lease.path, &["checkout", "-q", &slice]);
        if conflicting {
            commit_file(&lease.path, "README.md", "mine\n", "mine");
        } else {
            commit_file(&lease.path, "mine.txt", "mine\n", "mine");
        }
        let head_before = git(&lease.path, &["rev-parse", "HEAD"]);
        let (command, expect_failure) = match op {
            Op::ConflictMerge => (vec!["merge", "--no-commit", "other"], true),
            Op::CleanMerge => (vec!["merge", "--no-commit", "other"], false),
            Op::ConflictCherryPick | Op::ResolvedCherryPick => (vec!["cherry-pick", "other"], true),
        };
        let output = git_output(&lease.path, &command);
        assert_eq!(
            !output.status.success(),
            expect_failure,
            "{name}: {command:?}"
        );
        if matches!(op, Op::ResolvedCherryPick) {
            std::fs::write(lease.path.join("README.md"), "resolved\n").unwrap();
            git(&lease.path, &["add", "README.md"]);
            assert_eq!(
                git(&lease.path, &["ls-files", "-u"]),
                "",
                "{name}: resolved"
            );
        }
        assert!(
            git_output(&lease.path, &["rev-parse", "-q", "--verify", pseudo_ref])
                .status
                .success(),
            "{name}: {pseudo_ref} present before the report"
        );

        fx.complete(&worker, &task.id).await;
        let settled = fx.wait_settled(&task.id).await;

        let (result, _) = settled_result(&settled);
        let (code, _, retry_allowed) = failure_of(result);
        assert_eq!(code, DeliveryFailureCode::ProvenanceMismatch, "{name}");
        assert!(retry_allowed, "{name}");
        assert_eq!(result_code(&fx, &task.id).await, Some(15), "{name}");
        // The row's reason: the fixed sentence plus the evidence the script printed.
        let row = fx.delivery_row(&task.id).await.unwrap();
        let reason = row.failure_reason.clone().unwrap();
        assert!(
            reason.starts_with(&failure_sentence("15")),
            "{name}: {reason}"
        );
        let evidence = reason.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
        if unmerged_evidence {
            assert!(
                evidence.contains("README.md"),
                "{name}: unmerged entries: {reason}"
            );
            assert!(
                !evidence.contains(pseudo_ref),
                "{name}: `ls-files -u` exits first: {reason}"
            );
        } else {
            assert_eq!(evidence, pseudo_ref, "{name}: {reason}");
        }
        assert_eq!(
            git(&lease.path, &["rev-parse", "HEAD"]),
            head_before,
            "{name}: no new commit"
        );
        assert!(
            git_output(&lease.path, &["rev-parse", "-q", "--verify", pseudo_ref])
                .status
                .success(),
            "{name}: {pseudo_ref} still present"
        );
        let ref_name = format!(
            "refs/neige/candidates/{}/{}/{}",
            fx.track(),
            worker.card_id,
            row.delivery_id
        );
        assert_eq!(
            ref_target(&lease.git_common_dir, &ref_name),
            None,
            "{name}: no ref"
        );
        assert!(fx.candidate_row(&task.id).await.is_none(), "{name}");
        if unmerged_evidence {
            assert!(
                std::fs::read_to_string(lease.path.join("README.md"))
                    .unwrap()
                    .contains("<<<<<<<"),
                "{name}: the conflict markers were never committed"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// A28: a rebased lease tip records `base_is_ancestor = false`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rebased_lease_tip_records_base_not_ancestor() {
    let fx = fixture_with(|tmp| {
        let repo = tmp.join("repo");
        init_repo(&repo);
        // R = initial, B = base (HEAD): the lease is taken at B.
        commit_file(&repo, "base.txt", "base\n", "base");
        repo
    })
    .await;
    let rebased = fx.new_worker("rebased", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&rebased.card_id).await;
    let base = lease.base_sha.clone();
    let root = git(&fx.track_root, &["rev-list", "--max-parents=0", "HEAD"]);
    // A new commit R' on top of R, then the lease branch is rebased onto it: B is no ancestor.
    git(&fx.track_root, &["checkout", "-q", "-b", "alt", &root]);
    let alt = commit_file(&fx.track_root, "alt.txt", "alt\n", "alt");
    git(&fx.track_root, &["checkout", "-q", "main"]);
    git(&lease.path, &["rebase", "-q", "--onto", &alt, &base]);
    assert_eq!(git(&lease.path, &["rev-parse", "HEAD"]), alt);
    assert!(
        !git_output(&lease.path, &["merge-base", "--is-ancestor", &base, "HEAD"])
            .status
            .success()
    );
    let task = fx
        .running_task("rebased", "codex", &rebased.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "rebased\n").unwrap();

    fx.complete(&rebased, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;

    let (result, _) = settled_result(&settled);
    let (_, commit_sha, base_sha, base_is_ancestor) = candidate_of(result);
    assert_eq!(base_sha, base);
    assert!(!base_is_ancestor, "the observation, not a refusal");
    let committed = fx.worktree_committed_events(&rebased.card_id).await;
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0]["base_is_ancestor"], false);
    assert_eq!(committed[0]["commit_sha"], commit_sha);
    let candidate = fx.candidate_row(&task.id).await.expect("delivered");
    assert_eq!(candidate.base_is_ancestor, 0);
    assert_eq!(candidate.base_sha, base);
    let entry = fx.plan_entry("rebased").await;
    assert_eq!(
        entry["candidate"]["delivery"]["base_is_ancestor"], false,
        "{entry}"
    );

    // The positive case: an unrebased lease reads `true`.
    let plain = fx.new_worker("plain", AgentProvider::Codex).await;
    let plain_lease = fx.kernel_lease(&plain.card_id).await;
    let plain_task = fx
        .running_task("plain", "codex", &plain.card_id, json!({}))
        .await;
    std::fs::write(plain_lease.path.join("worker.txt"), "plain\n").unwrap();
    fx.complete(&plain, &plain_task.id).await;
    let settled = fx.wait_settled(&plain_task.id).await;
    let (_, _, _, base_is_ancestor) = candidate_of(settled_result(&settled).0);
    assert!(base_is_ancestor);
    assert_eq!(
        fx.candidate_row(&plain_task.id)
            .await
            .unwrap()
            .base_is_ancestor,
        1
    );
}

// ---------------------------------------------------------------------------
// A30: an observation failure inside the script is `commit_failed`, never a verdict.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn observation_failure_settles_as_commit_failed() {
    let fx = fixture().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("observed", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "observed\n").unwrap();

    // A `git` in front of the real one: `worktree list` prints its matching lines, then exits 128.
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git on PATH");
    let bin = fx.track_root.parent().unwrap().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_executable(
        &bin.join("git"),
        &format!(
            "#!/bin/sh\nREAL='{}'\nif [ \"$1\" = worktree ] && [ \"$2\" = list ]; then \"$REAL\" \"$@\"; exit 128; fi\nexec \"$REAL\" \"$@\"\n",
            real_git.display()
        ),
    );
    let original_path = std::env::var_os("PATH").unwrap();
    let mut dirs = vec![bin];
    dirs.extend(std::env::split_paths(&original_path));
    // The forge wrapper inherits this process's PATH; one process per test under nextest.
    unsafe { std::env::set_var("PATH", std::env::join_paths(dirs).unwrap()) };

    fx.complete(&worker, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;
    unsafe { std::env::set_var("PATH", original_path) };

    let (result, _) = settled_result(&settled);
    let (code, reason, retry_allowed) = failure_of(result);
    assert_eq!(code, DeliveryFailureCode::CommitFailed);
    assert_eq!(reason, failure_sentence("14"));
    assert!(retry_allowed);
    assert_eq!(result_code(&fx, &task.id).await, Some(14));
    let row = fx.delivery_row(&task.id).await.unwrap();
    let ref_name = format!(
        "refs/neige/candidates/{}/{}/{}",
        fx.track(),
        worker.card_id,
        row.delivery_id
    );
    assert_eq!(ref_target(&lease.git_common_dir, &ref_name), None, "no ref");
    assert!(fx.candidate_row(&task.id).await.is_none());
    assert_eq!(
        git(&lease.path, &["status", "--porcelain"]),
        "?? worker.txt",
        "nothing staged"
    );
    assert_eq!(
        git(&lease.path, &["rev-parse", "HEAD"]),
        lease.base_sha,
        "no commit"
    );
}

// ---------------------------------------------------------------------------
// The workspace is gone: `workspace_missing`, recorded not retryable, no retained path in the wake.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_missing_delivery_is_not_retryable() {
    let mut fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("gone", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "gone\n").unwrap();
    fx.report_only(&worker, &task.id).await;
    let planner = fx.planner().await;
    // The lease directory is removed before the delivery ever runs.
    std::fs::remove_dir_all(&lease.path).unwrap();

    fx.reboot().await;
    let settled = fx.wait_settled(&task.id).await;

    let (result, wake_reason) = settled_result(&settled);
    let (code, reason, retry_allowed) = failure_of(result);
    assert_eq!(code, DeliveryFailureCode::WorkspaceMissing);
    assert_eq!(reason, failure_sentence("workspace_missing"));
    assert!(!retry_allowed);
    assert_eq!(wake_reason, DeliveryWakeReason::Failed);
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(row.failure_code.as_deref(), Some("workspace_missing"));
    assert_eq!(row.retry_allowed, Some(0));
    let pending = wait_observations(&planner, 1).await;
    let Observation::TaskGitDeliverySettled { retained_path, .. } = &pending[0] else {
        panic!("{pending:?}");
    };
    assert_eq!(retained_path, &None);
    let text = pending[0].to_turn_text();
    assert!(!text.contains("Files retained at"), "{text}");
    assert!(text.contains("(workspace_missing)"), "{text}");
    assert!(!text.contains("task.delivery"), "{text}");
    assert!(!text.to_ascii_lowercase().contains("retr"), "{text}");
}

// ---------------------------------------------------------------------------
// A23 / A23b / A23c: wake discipline.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ungated_candidate_settlement_wakes_planner_once() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("wake-once", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "wake\n").unwrap();

    fx.complete(&worker, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;
    assert_eq!(
        settled_result(&settled).1,
        DeliveryWakeReason::UngatedCandidate
    );

    // Exactly one turn, and it is the settlement — `task.completed` (persisted between the push
    // cursor and the settlement) is suppressed both live and in the settlement's prefix replay.
    let pending = wait_observations(&planner, 1).await;
    assert!(
        matches!(&pending[0], Observation::TaskGitDeliverySettled { attempt_id, result: DeliverySettlement::Candidate { .. }, .. } if attempt_id == &task.id),
        "{pending:?}"
    );
    let completed = fx
        .boot
        .repo
        .events_for_track(fx.track(), &["task.completed"], None)
        .await
        .unwrap();
    assert_eq!(completed.len(), 1);
    assert!(
        completed[0].id < settled.id,
        "the self-report sits before the settlement"
    );
    for _ in 0..2 {
        fx.dispatcher
            .catch_up_push(fx.boot.track_id.clone(), settled.event.clone(), settled.id)
            .await;
    }
    let pending = assert_observations_settle_at(&planner, 1).await;
    assert!(
        !pending
            .iter()
            .any(|o| matches!(o, Observation::TaskCompleted { .. })),
        "{pending:?}"
    );
}

/// A gate step that blocks until `flag` exists.
pub(super) fn gate_waiting_for(flag: &Path) -> Value {
    json!({"steps": [{"name": "wait", "cmd": format!("until [ -f '{}' ]; do sleep 0.1; done", flag.display())}], "timeout_secs": 60})
}

pub(super) async fn wait_gate_result(fx: &Fx, attempt: &str) -> calm_server::db::TrackEvent {
    tokio::time::timeout(WAIT, async {
        loop {
            let rows = fx
                .boot
                .repo
                .events_for_track(fx.track(), &["task.gate_result"], None)
                .await
                .unwrap();
            if let Some(row) = rows.into_iter().find(|row| matches!(&row.event, Event::TaskGateResult { task_id, .. } if task_id == attempt)) {
                break row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("gate result")
}

/// D12 (i): a gate flipped the row before its delivery settled. Since slice 4 the gate is
/// admitted only after settlement, so the window is played by hand: the row is flipped to
/// `done` with a recorded verdict the way a slice 2/3 gate left it, then the held delivery
/// settles against it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_settlement_wakes_when_gate_already_flipped() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    // The delivery's commit blocks in a pre-commit hook until the flag appears.
    let flag = fx.track_root.parent().unwrap().join("commit-may-proceed");
    let hooks = lease.git_common_dir.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    write_executable(
        &hooks.join("pre-commit"),
        &format!(
            "#!/bin/sh\nuntil [ -f '{}' ]; do sleep 0.1; done\nexit 0\n",
            flag.display()
        ),
    );
    let task = fx
        .running_task(
            "gate-first",
            "codex",
            &worker.card_id,
            json!({"gate": {"steps": [{"name": "t", "cmd": "true"}]}, "no_gate_reason": null}),
        )
        .await;
    assert!(task.gate_json.is_some());
    std::fs::write(lease.path.join("worker.txt"), "gated\n").unwrap();

    fx.complete(&worker, &task.id).await;
    assert_eq!(
        current(&fx.boot, "gate-first").await.status,
        TaskStatus::Verifying
    );
    // Slice 4 admission: no gate while the delivery is pending — the flip is the window's.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        fx.runtime
            .find_by_kind_and_idempotency(TASK_VERIFY_KIND, &format!("{}#g1", task.id))
            .await
            .unwrap()
            .is_none(),
        "the gate waits for settlement"
    );
    sqlx::query(
        "UPDATE tasks SET status = 'done', gate_attempt = 1, gate_result_json = ?1, finished_at_ms = ?2 WHERE id = ?3",
    )
    .bind(json!({"passed": true, "exit_code": 0, "log_tail": "", "log_path": "/l", "attempt": 1}).to_string())
    .bind(now_ms())
    .bind(&task.id)
    .execute(&fx.pool())
    .await
    .unwrap();
    assert_observations_settle_at(&planner, 0).await;
    assert!(
        fx.settled_events_for(&task.id).await.is_empty(),
        "the commit is still blocked"
    );

    std::fs::write(&flag, b"").unwrap();
    let settled = fx.wait_settled(&task.id).await;
    let (result, wake_reason) = settled_result(&settled);
    candidate_of(result);
    assert_eq!(wake_reason, DeliveryWakeReason::GateAlreadyTerminal);
    assert_eq!(
        fx.delivery_row(&task.id)
            .await
            .unwrap()
            .wake_reason
            .as_deref(),
        Some("gate_already_terminal")
    );

    // The whole lifecycle: the one `gate_already_terminal` settlement turn (the hand-flipped
    // verdict is the window's, not a gate result of this build).
    let pending = wait_observations(&planner, 1).await;
    assert!(
        matches!(
            &pending[0],
            Observation::TaskGitDeliverySettled {
                result: DeliverySettlement::Candidate { .. },
                ..
            }
        ),
        "{pending:?}"
    );
    assert_observations_settle_at(&planner, 1).await;
    assert!(fx.events_for(GATE_RESULT_KIND, &task.id).await.is_empty());
    assert_eq!(
        current(&fx.boot, "gate-first").await.status,
        TaskStatus::Done
    );
    let entry = fx.plan_entry("gate-first").await;
    assert_eq!(
        entry["candidate"]["delivery"]["state"], "committed",
        "{entry}"
    );
    assert_eq!(entry["candidate"]["verification"]["state"], "unbound");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settlement_wake_is_replay_stable() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let task = fx
        .running_task(
            "deferred",
            "codex",
            &worker.card_id,
            json!({"gate": gate_waiting_for(&flag), "no_gate_reason": null}),
        )
        .await;
    std::fs::write(lease.path.join("worker.txt"), "deferred\n").unwrap();

    // The candidate settles while the row is `verifying`: deferred to the gate, zero turns.
    fx.complete(&worker, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;
    let (result, wake_reason) = settled_result(&settled);
    candidate_of(result);
    assert_eq!(wake_reason, DeliveryWakeReason::DeferredToGate);
    assert_eq!(
        current(&fx.boot, "deferred").await.status,
        TaskStatus::Verifying
    );
    assert_observations_settle_at(&planner, 0).await;

    // The gate finishes: one turn.
    std::fs::write(&flag, b"").unwrap();
    let gate = wait_gate_result(&fx, &task.id).await;
    assert!(gate.id > settled.id);
    let live = wait_observations(&planner, 1).await;
    assert!(
        matches!(&live[0], Observation::TaskGateResult { passed: true, .. }),
        "{live:?}"
    );
    assert_observations_settle_at(&planner, 1).await;
    assert_eq!(current(&fx.boot, "deferred").await.status, TaskStatus::Done);

    // The kernel dies before the harness persisted either delivery: the snapshot's watermark
    // sits before the settlement event. Boot catch-up replays from there.
    let planner_session = planner_identity(&fx.boot).session_id;
    planner.shutdown().await.unwrap();
    fx.harness.remove(&planner_session);
    let mut snapshot = HarnessSnapshot::initial(settled.id - 1, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some("planner-observer".into());
    let mut tx = fx.pool().begin().await.unwrap();
    session_set_handle_state_tx(
        &mut tx,
        &planner_session,
        Some(serde_json::to_value(&snapshot).unwrap()),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let registry = HarnessRegistry::new();
    let areas = calm_server::track_area_cache::TrackAreaCache::new();
    fx.boot.repo.seed_track_area_cache(&areas).await.unwrap();
    let claude_wiring =
        calm_server::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(
            fx.boot.repo.clone(),
        );
    let recovery_daemon =
        SharedCodexAppServer::new_fake_running_with_pending(fx.boot.repo.clone(), None);
    let recovered = recover_harnesses_on_boot(
        fx.boot.repo.clone(),
        EventBus::new(),
        fx.boot.card_role_cache.clone(),
        areas,
        recovery_daemon.clone(),
        recovery_daemon.thread_seals().clone(),
        &claude_wiring,
        &registry,
        &calm_server::harness::new_track_delete_locks(),
        calm_server::harness::BootRows::All,
    )
    .await
    .unwrap();
    assert_eq!(recovered, 1);
    let stored = fx
        .boot
        .repo
        .session_projection_by_id(&planner_session)
        .await
        .unwrap()
        .unwrap();
    let stored: HarnessSnapshot =
        serde_json::from_value(stored.handle_state_json.unwrap()).unwrap();
    let replayed = stored.pending_observations();
    assert_eq!(
        replayed.len(),
        1,
        "the row is `done` now, the event still says deferred: {replayed:?}"
    );
    assert!(
        matches!(
            &replayed[0],
            Observation::TaskGateResult { passed: true, .. }
        ),
        "{replayed:?}"
    );
    assert_eq!(
        replayed
            .iter()
            .map(std::mem::discriminant)
            .collect::<Vec<_>>(),
        live.iter().map(std::mem::discriminant).collect::<Vec<_>>(),
        "live and replay agree"
    );
    if let Some(handle) = registry.remove(&planner_session) {
        handle.shutdown().await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// A26: a slice 1 lease (base recorded, no delivery policy) gets no delivery.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slice1_lease_completing_after_slice2_stays_legacy() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    for (name, provider) in [
        ("legacy-codex", AgentProvider::Codex),
        ("legacy-claude", AgentProvider::Claude),
    ] {
        let worker = fx.new_worker(name, provider.clone()).await;
        let lease = fx.kernel_lease(&worker.card_id).await;
        sqlx::query("UPDATE workspace_leases SET delivery_policy = NULL WHERE lease_id = ?1")
            .bind(&lease.lease_id)
            .execute(&fx.pool())
            .await
            .unwrap();
        let (base_sha, policy): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT base_sha, delivery_policy FROM workspace_leases WHERE lease_id = ?1",
        )
        .bind(&lease.lease_id)
        .fetch_one(&fx.pool())
        .await
        .unwrap();
        assert_eq!(base_sha.as_deref(), Some(lease.base_sha.as_str()));
        assert_eq!(policy, None);
        let task = fx
            .running_task(name, "codex", &worker.card_id, json!({}))
            .await;
        std::fs::write(lease.path.join("worker.txt"), "legacy\n").unwrap();

        fx.complete(&worker, &task.id).await;

        assert_eq!(
            fx.delivery_count(&task.id).await,
            0,
            "{name}: no delivery row"
        );
        // The legacy `git.commit:auto` is gone (#1830 S2 D12): nothing commits a legacy lease.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(fx.forge_op_count().await, 0, "{name}: nothing commits it");
        assert_eq!(
            git(&lease.path, &["rev-parse", "HEAD"]),
            lease.base_sha,
            "{name}"
        );
        assert!(fx.candidate_row(&task.id).await.is_none());
        let entry = fx.plan_entry(name).await;
        assert_eq!(entry["candidate"]["binding"], "unbound", "{name}: {entry}");
        assert_eq!(entry["candidate"]["reason"], "legacy_lease");
        assert!(
            entry["candidate"].get("delivery").is_none(),
            "{name}: {entry}"
        );
        let summary = fx.plan_summary_entry(name).await;
        assert_eq!(
            summary["candidate"]["binding"], "unbound",
            "{name}: {summary}"
        );
        assert_eq!(summary["candidate"]["reason"], "legacy_lease");
        // The next worker needs a clean tree (#1830 S2 D6).
        std::fs::remove_file(lease.path.join("worker.txt")).unwrap();
    }
    // `task.completed` is pushed as today for both.
    let pending = wait_observations(&planner, 2).await;
    assert!(
        pending
            .iter()
            .all(|o| matches!(o, Observation::TaskCompleted { .. })),
        "{pending:?}"
    );
    assert_observations_settle_at(&planner, 2).await;
    assert!(fx.settled_events().await.is_empty());

    // Slice 4: a gated legacy-lease attempt runs its gate as today — admitted at once (no
    // delivery to wait for), frozen and recorded `Unbound { LegacyLease }`, no sample taken.
    let worker = fx.new_worker("legacy-gated", AgentProvider::Claude).await;
    let lease = fx.kernel_lease(&worker.card_id).await;
    sqlx::query("UPDATE workspace_leases SET delivery_policy = NULL WHERE lease_id = ?1")
        .bind(&lease.lease_id)
        .execute(&fx.pool())
        .await
        .unwrap();
    let task = fx
        .running_task(
            "legacy-gated",
            "claude",
            &worker.card_id,
            json!({"gate": {"steps": [{"name": "t", "cmd": "true"}]}, "no_gate_reason": null}),
        )
        .await;
    std::fs::write(lease.path.join("worker.txt"), "legacy\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let gate = wait_gate_result(&fx, &task.id).await;
    let Event::TaskGateResult {
        passed,
        target,
        status_detail,
        ..
    } = &gate.event
    else {
        panic!("{gate:?}");
    };
    assert!(passed, "{gate:?}");
    assert_eq!(status_detail, &None);
    assert_eq!(
        target,
        &Some(calm_types::verify_target::VerifyTarget::Unbound {
            reason: calm_types::verify_target::UnboundReason::LegacyLease
        })
    );
    assert_eq!(fx.delivery_count(&task.id).await, 0);
    assert_eq!(
        current(&fx.boot, "legacy-gated").await.status,
        TaskStatus::Done
    );
    let pending = wait_observations(&planner, 3).await;
    assert!(
        matches!(
            &pending[2],
            Observation::TaskGateResult { passed: true, .. }
        ),
        "{pending:?}"
    );
    assert_observations_exactly(&planner, 3).await;
}

// ---------------------------------------------------------------------------
// A31: the read surface is total over every current attempt of a Track.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_view_is_total_over_task_status() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();

    // `running` on a kernel lease, never reported.
    let running = fx.new_worker("running", AgentProvider::Codex).await;
    fx.kernel_lease(&running.card_id).await;
    fx.running_task("running", "codex", &running.card_id, json!({}))
        .await;
    fx.release_lease_by_hand(&running.card_id).await;

    // `pending`, never claimed (its dependency keeps it out of the ready set).
    declare(
        &fx.boot,
        json!({"key": "pending", "kind": "codex", "goal": "wait", "depends_on": ["running"],
            "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true, "no_gate_reason": "fixture"}),
    )
    .await;

    // `failed/spawn-failed` with a lease and no delivery row.
    let spawn_failed = fx.new_worker("spawn-failed", AgentProvider::Codex).await;
    fx.kernel_lease(&spawn_failed.card_id).await;
    let task = fx
        .running_task("spawn-failed", "codex", &spawn_failed.card_id, json!({}))
        .await;
    sqlx::query("UPDATE tasks SET status = 'failed', status_detail = 'spawn-failed', finished_at_ms = ?2 WHERE id = ?1")
        .bind(&task.id)
        .bind(now_ms())
        .execute(&fx.pool())
        .await
        .unwrap();
    fx.release_lease_by_hand(&spawn_failed.card_id).await;

    // `failed/worker-timeout`, gated.
    let timed_out = fx.new_worker("timed-out", AgentProvider::Codex).await;
    fx.kernel_lease(&timed_out.card_id).await;
    let task = fx
        .running_task(
            "timed-out",
            "codex",
            &timed_out.card_id,
            json!({"gate": {"steps": [{"name": "t", "cmd": "true"}]}, "no_gate_reason": null}),
        )
        .await;
    sqlx::query("UPDATE tasks SET status = 'failed', status_detail = 'worker-timeout', finished_at_ms = ?2 WHERE id = ?1")
        .bind(&task.id)
        .bind(now_ms())
        .execute(&fx.pool())
        .await
        .unwrap();
    fx.release_lease_by_hand(&timed_out.card_id).await;

    // `done` whose delivery row was deleted.
    let done = fx.new_worker("done", AgentProvider::Codex).await;
    fx.kernel_lease(&done.card_id).await;
    let task = fx
        .running_task("done", "codex", &done.card_id, json!({}))
        .await;
    fx.report_only(&done, &task.id).await;
    assert_eq!(fx.delivery_count(&task.id).await, 1);
    sqlx::query("DELETE FROM task_git_deliveries WHERE producer_attempt_id = ?1")
        .bind(&task.id)
        .execute(&fx.pool())
        .await
        .unwrap();
    assert_eq!(current(&fx.boot, "done").await.status, TaskStatus::Done);

    let list = call_tool(
        &fx.boot,
        "neige_task_ls",
        planner_identity(&fx.boot),
        json!({"detail": "full"}),
    )
    .await
    .unwrap();
    let entry = |key: &str| {
        list["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["key"] == key)
            .cloned()
            .unwrap_or_else(|| panic!("{key}: {list}"))
    };
    let pending = entry("pending");
    assert_eq!(pending["status"], "pending", "{pending}");
    assert_eq!(
        pending["candidate"],
        json!({"binding": "none", "reason": "no_lease"}),
        "{pending}"
    );

    let running_entry = entry("running");
    assert_eq!(
        running_entry["candidate"]["binding"], "bound",
        "{running_entry}"
    );
    assert_eq!(
        running_entry["candidate"]["delivery"],
        json!({"state": "not_reported"}),
        "{running_entry}"
    );

    let spawn_failed_entry = entry("spawn-failed");
    assert_eq!(
        spawn_failed_entry["candidate"]["binding"], "bound",
        "{spawn_failed_entry}"
    );
    assert_eq!(
        spawn_failed_entry["candidate"]["delivery"],
        json!({"state": "ended_without_delivery", "attempt_status": "failed", "status_detail": "spawn-failed"}),
        "{spawn_failed_entry}"
    );

    let timed_out_entry = entry("timed-out");
    assert_eq!(
        timed_out_entry["candidate"]["binding"], "bound",
        "{timed_out_entry}"
    );
    assert_eq!(
        timed_out_entry["candidate"]["delivery"],
        json!({"state": "ended_without_delivery", "attempt_status": "failed", "status_detail": "worker-timeout"}),
        "{timed_out_entry}"
    );

    let done_entry = entry("done");
    assert_eq!(done_entry["candidate"]["binding"], "bound", "{done_entry}");
    assert_eq!(
        done_entry["candidate"]["delivery"],
        json!({"state": "inconsistent", "mismatches": ["delivery_row_missing"]}),
        "{done_entry}"
    );

    for key in ["pending", "running", "spawn-failed", "timed-out", "done"] {
        let candidate = &entry(key)["candidate"];
        assert_ne!(candidate["binding"], "unbound", "{key}: {candidate}");
        assert_ne!(
            candidate["delivery"]["state"], "pending",
            "{key}: {candidate}"
        );
        assert!(
            candidate["delivery"].get("delivery_id").is_none(),
            "{key}: {candidate}"
        );
        assert!(!candidate.is_null(), "{key}");
    }
    let summary = fx.plan_summary_entry("running").await;
    assert_eq!(
        summary["candidate"]["delivery"]["state"], "not_reported",
        "{summary}"
    );
}

// ===========================================================================
// #1893 S6: a failed delivery fails its gated task; nothing retries or abandons it.
// ===========================================================================

/// The delivery of a gated attempt fails: the settlement flips the `verifying` row to
/// `failed/delivery-failed` in its own transaction and appends one kernel `task.failed` after the
/// settlement event. The Track's budget is free, no gate is admitted, and the Planner wakes once
/// — with the settlement, not the `task.failed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_delivery_fails_the_gated_task() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    install_pre_commit(&lease, HOOK_EXIT_1);
    let task = fx
        .running_task(
            "first",
            "codex",
            &worker.card_id,
            json!({"gate": gate_waiting_for(&flag), "no_gate_reason": null}),
        )
        .await;
    let task_failed_handled = fx.arm_task_failed_barrier(&task.id);
    std::fs::write(lease.path.join("worker.txt"), "rejected\n").unwrap();
    fx.complete(&worker, &task.id).await;

    let settled = fx.wait_settled(&task.id).await;
    let (code, _, _) = failure_of(settled_result(&settled).0);
    assert_eq!(code, DeliveryFailureCode::CommitFailed);
    let columns = fx.task_columns(&task.id).await;
    assert_eq!(columns.status, TaskStatus::Failed, "{columns:?}");
    assert_eq!(columns.status_detail.as_deref(), Some("delivery-failed"));
    assert!(columns.finished_at_ms.is_some());
    let failed = fx.events_for(TASK_FAILED_KIND, &task.id).await;
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
        matches!(
            &failed[0].actor,
            calm_server::ids::ActorId::KernelDispatcher
        ),
        "{:?}",
        failed[0].actor
    );
    assert!(
        matches!(&failed[0].event, Event::TaskFailed { reason, .. } if reason == "delivery-failed"),
        "{:?}",
        failed[0].event
    );
    assert!(
        failed[0].id > settled.id,
        "the task.failed follows the settlement"
    );
    fx.assert_no_gate_op(&task.id).await;
    let entry = fx.plan_entry("first").await;
    assert_eq!(entry["candidate"]["delivery"]["state"], "failed", "{entry}");
    assert_eq!(
        entry["candidate"]["verification"]["state"], "not_reached",
        "{entry}"
    );

    // The default budget is 1 and the failed row no longer holds it: the next task is claimed
    // (the spawn itself has no adapter here).
    declare(
        &fx.boot,
        json!({"key": "second", "kind": "codex", "goal": "next", "declared_by": PLANNER_DECLARATION_AUTHOR,
            "ready": true, "no_gate_reason": "budget fixture"}),
    )
    .await;
    fx.scheduler()
        .schedule_track(fx.boot.track_id.clone())
        .await;
    let second = tokio::time::timeout(WAIT, async {
        loop {
            let task = current(&fx.boot, "second").await;
            if task.status != TaskStatus::Pending {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the second task leaves pending");
    assert_eq!(second.status, TaskStatus::Dispatched, "{second:?}");

    // One turn: the settlement. The Dispatcher has handled the `task.failed` (barrier) and the
    // harness ingress is drained before the count is read.
    wait_observations(&planner, 1).await;
    wait_task_failed_handled(&task_failed_handled).await;
    let pending = assert_observations_exactly(&planner, 1).await;
    assert!(
        matches!(
            &pending[0],
            Observation::TaskGitDeliverySettled {
                result: DeliverySettlement::Failed { .. },
                ..
            }
        ),
        "{pending:?}"
    );
}

// ---------------------------------------------------------------------------
// A9d / A6c: Track and Area deletion after a failed delivery; candidate refs go with the Track.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn track_and_area_delete_after_a_failed_delivery() {
    for delete_area in [false, true] {
        let fx = fixture().await;
        let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
        let (_, task, lease) = fx
            .hook_failing_task(
                "fail-then-delete",
                json!({"gate": gate_waiting_for(&flag), "no_gate_reason": null}),
            )
            .await;
        fx.wait_settled(&task.id).await;
        assert_eq!(fx.task_columns(&task.id).await.status, TaskStatus::Failed);
        fx.assert_no_gate_op(&task.id).await;
        // A second worker delivers a candidate on the same Track: its ref must go with the Track.
        remove_pre_commit(&lease);
        undo_worker_changes(&lease.path);
        let other = fx.new_worker("delivers", AgentProvider::Codex).await;
        let other_lease = fx.kernel_lease(&other.card_id).await;
        let other_task = fx
            .running_task("delivers", "codex", &other.card_id, json!({}))
            .await;
        std::fs::write(other_lease.path.join("worker.txt"), "ok\n").unwrap();
        fx.complete(&other, &other_task.id).await;
        candidate_of(settled_result(&fx.wait_settled(&other_task.id).await).0);
        assert_eq!(candidate_refs(&lease.git_common_dir, fx.track()).len(), 1);
        for table in ["task_git_deliveries", "task_candidates"] {
            assert!(fx.table_count(table).await > 0, "{table}");
        }
        let events_before: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE scope_track = ?1")
                .bind(fx.track())
                .fetch_one(&fx.pool())
                .await
                .unwrap();
        assert!(events_before > 0);

        let path = if delete_area {
            format!("/api/areas/{}", fx.boot.area_id.as_str())
        } else {
            format!("/api/tracks/{}", fx.track())
        };
        let (status, body) = fx.http_delete(&path).await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{path}: {body}");
        // The two delivery tables and the leases.
        for table in ["task_git_deliveries", "task_candidates", "workspace_leases"] {
            assert_eq!(fx.table_count(table).await, 0, "{path}: {table}");
        }
        let events_after: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE scope_track = ?1")
                .bind(fx.track())
                .fetch_one(&fx.pool())
                .await
                .unwrap();
        assert!(
            events_after >= events_before,
            "{path}: events outlive the rows ({events_after} < {events_before})"
        );
        assert_eq!(
            candidate_refs(&lease.git_common_dir, fx.track()),
            Vec::<String>::new(),
            "{path}: the candidate ref prefix is empty"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_refs_are_deleted_with_the_track() {
    let fx = fixture_with(linked_worktree_track).await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let main_git = fx
        .track_root
        .parent()
        .unwrap()
        .join("main")
        .join(".git")
        .canonicalize()
        .unwrap();
    assert_eq!(lease.git_common_dir, main_git);
    let task = fx
        .running_task("moved-then-deleted", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "moved\n").unwrap();
    fx.complete(&worker, &task.id).await;
    candidate_of(settled_result(&fx.wait_settled(&task.id).await).0);
    let candidate = fx.candidate_row(&task.id).await.unwrap();
    assert_eq!(
        candidate_refs(&main_git, fx.track()),
        vec![candidate.ref_name.clone()]
    );

    // The Track cwd (a linked worktree, with the lease under it) moves away before the delete:
    // `git -C <repo_root>` would fail; the common dir still answers.
    let moved = fx.track_root.with_file_name("track-wt-moved");
    std::fs::rename(&fx.track_root, &moved).unwrap();
    assert!(!fx.track_root.exists());

    let (status, body) = fx.http_delete(&format!("/api/tracks/{}", fx.track())).await;
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        candidate_refs(&main_git, fx.track()),
        Vec::<String>::new(),
        "for-each-ref is empty"
    );
    assert_eq!(ref_target(&main_git, &candidate.ref_name), None);
    assert_eq!(fx.table_count("task_candidates").await, 0);
}
