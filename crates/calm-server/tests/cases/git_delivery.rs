//! #1727 S4 slice 2 PR-B: kernel git delivery for attached workers, wired end to end — the
//! report transaction's delivery row, the keyed forge submission, the scheduler's settlement
//! into `task_candidates` + `task.git_delivery_settled`, the deferred self-report and the
//! `calm.plan.list.candidate` read surface. Design §6 rows A3–A7, A23–A23c, A25–A31.
//!
//! Slice 3 (`calm.task.delivery{retry|abandon}`, the abandonment table, the candidate-ref
//! cleanup on Track delete, the decision clause of the failed wake): §6 rows A8a–A8d, A9–A9d,
//! A23b (first fixture), A25b (positive case), A6c (deletion assertion) — the second half of
//! this file.
//!
//! One process per test (nextest): the PATH mutation in `observation_failure_settles_as_commit_failed`
//! relies on that.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::mcp_track_report::{Boot, boot, call_tool, planner_identity, worker_identity};
use crate::task_recovery::{current, declare};
use calm_server::db::sqlite::{
    begin_immediate_tx, card_create_with_id_tx, session_set_handle_state_tx,
    session_start_runtime_tx,
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
use calm_server::model::{CardRole, NewCard, NewTrack, Task, TaskStatus, TrackLifecycle, now_ms};
use calm_server::operation::forge_action_adapter::{FORGE_ACTION_KIND, ForgeActionAdapter};
use calm_server::operation::task_verify_adapter::{TASK_VERIFY_KIND, TaskVerifyAdapter};
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
};
use calm_server::plugin_host::mcp::RpcError;
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
pub(super) const TOOL_TASK_DELIVERY: &str = "calm.task.delivery";
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
    pub(super) predecessor_delivery_id: Option<String>,
    pub(super) request_idempotency_key: Option<String>,
    pub(super) reason: Option<String>,
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

    let events = boot.ctx.events.clone();
    let operation_repo = Arc::new(SqlxOperationRepo::new(pool.clone()));
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = boot.repo.clone();
    let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo.clone());
    let completion = OperationCompletionBus::new();
    let daemon = Arc::new(DaemonClient::new_stub());
    let spawn_ctx = SpawnCtx::new(
        route_repo,
        operation_repo.clone(),
        daemon.clone(),
        terminal_renderer.clone(),
        events.clone(),
        completion.clone(),
    );
    let gate_logs_dir = boot.ctx.gate_logs_dir.clone();
    let mut adapters = vec![
        Arc::new(ForgeActionAdapter::new()) as Arc<dyn ProviderAdapter>,
        Arc::new(TaskVerifyAdapter::new(gate_logs_dir.clone())) as Arc<dyn ProviderAdapter>,
    ];
    adapters.extend(extra(&boot));
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
    // The tool-side scheduler poke (`calm.task.delivery{retry}`), as `AppState::new` binds it;
    // `respawn_dispatcher` repoints it at the new scheduler.
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
        workspace_root,
        _tmp: tmp,
    }
}

/// The tool-side scheduler triggers, forwarded to whichever scheduler `poke_target` names now.
struct RepointablePoke(Arc<std::sync::RwLock<Arc<calm_server::scheduler::Scheduler>>>);

impl calm_server::mcp_server::registry::SchedulerPokes for RepointablePoke {
    fn poke(&self, track: calm_server::ids::TrackId) {
        self.0.read().unwrap().poke(track);
    }

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
        calm_server::scheduler::DEFAULT_TRACK_TASK_BUDGET,
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

    /// The production lease sequence for `card`: prepare from the Track workspace, resolve the
    /// lease base (the upstream when the repo has one, else HEAD), the kernel-policy row, the
    /// worktree pinned to the base.
    pub(super) async fn kernel_lease(&self, card: &str) -> KernelWorkspaceLease {
        take_kernel_workspace_lease_for_test(&self.pool(), self.track(), card, &self.workspace_root)
            .await
            .unwrap()
    }

    pub(super) fn slice_branch(&self, card: &str) -> String {
        format!("neige/{}/{card}", self.track())
    }

    pub(super) async fn complete(&self, worker: &ToolCallIdentity, task_id: &str) {
        call_tool(
            &self.boot,
            "calm.task.complete",
            worker.clone(),
            json!({"idempotency_key": task_id, "result": {"ok": true}}),
        )
        .await
        .unwrap();
    }

    /// The report transaction alone — no forge submission — the state a kernel that died right
    /// after `calm.task.complete`'s transaction leaves behind (no crash seam: 5.1.16).
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
            "SELECT delivery_id, ordinal, operation_key, forge_idempotency_key, \
             predecessor_delivery_id, request_idempotency_key, reason, settlement, \
             settled_event_id, failure_code, failure_reason, retry_allowed, wake_reason \
             FROM task_git_deliveries WHERE producer_attempt_id = ?1 ORDER BY ordinal DESC LIMIT 1",
        )
        .bind(attempt)
        .fetch_optional(&self.pool())
        .await
        .unwrap()
    }

    /// The delivery row of one attempt at `ordinal`.
    pub(super) async fn delivery_row_at(
        &self,
        attempt: &str,
        ordinal: i64,
    ) -> Option<DeliveryRowView> {
        sqlx::query_as(
            "SELECT delivery_id, ordinal, operation_key, forge_idempotency_key, \
             predecessor_delivery_id, request_idempotency_key, reason, settlement, \
             settled_event_id, failure_code, failure_reason, retry_allowed, wake_reason \
             FROM task_git_deliveries WHERE producer_attempt_id = ?1 AND ordinal = ?2",
        )
        .bind(attempt)
        .bind(ordinal)
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

    /// `calm.plan.list` (full detail) entry of `key`.
    pub(super) async fn plan_entry(&self, key: &str) -> Value {
        let list = call_tool(
            &self.boot,
            "calm.plan.list",
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
            "calm.plan.list",
            planner_identity(&self.boot),
            json!({"detail": "summary", "key": key}),
        )
        .await
        .unwrap();
        list["tasks"][0].clone()
    }

    // -- slice 3 ---------------------------------------------------------------------------

    /// `calm.task.delivery` as the Planner.
    pub(super) async fn delivery_action(&self, args: Value) -> Result<Value, RpcError> {
        call_tool(
            &self.boot,
            TOOL_TASK_DELIVERY,
            planner_identity(&self.boot),
            args,
        )
        .await
    }

    pub(super) async fn retry(
        &self,
        task: &Task,
        delivery_id: &str,
        idempotency_key: &str,
        reason: Option<&str>,
    ) -> Result<Value, RpcError> {
        self.delivery_action(action_args(
            task,
            delivery_id,
            idempotency_key,
            "retry",
            reason,
        ))
        .await
    }

    pub(super) async fn abandon(
        &self,
        task: &Task,
        delivery_id: &str,
        idempotency_key: &str,
        reason: Option<&str>,
    ) -> Result<Value, RpcError> {
        self.delivery_action(action_args(
            task,
            delivery_id,
            idempotency_key,
            "abandon",
            reason,
        ))
        .await
    }

    pub(super) async fn abandonment_row(&self, delivery_id: &str) -> Option<AbandonmentRowView> {
        sqlx::query_as(
            "SELECT delivery_id, producer_attempt_id, request_idempotency_key, reason, \
             task_outcome, task_status FROM task_git_delivery_abandonments WHERE delivery_id = ?1",
        )
        .bind(delivery_id)
        .fetch_optional(&self.pool())
        .await
        .unwrap()
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

    /// Wait until the attempt has `n` settlement events; return the `n`-th (1-based).
    pub(super) async fn wait_settled_nth(
        &self,
        attempt: &str,
        n: usize,
    ) -> calm_server::db::TrackEvent {
        tokio::time::timeout(WAIT, async {
            loop {
                let rows = self.settled_events_for(attempt).await;
                if rows.len() >= n {
                    break rows.into_iter().nth(n - 1).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("attempt {attempt} never reached settlement {n}"))
    }

    /// The gate Operation of `attempt` (`<attempt>#g1`), once it exists.
    pub(super) async fn wait_gate_op(&self, attempt: &str) -> calm_server::operation::Operation {
        let key = format!("{attempt}#g1");
        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(op) = self
                    .runtime
                    .find_by_kind_and_idempotency(TASK_VERIFY_KIND, &key)
                    .await
                    .unwrap()
                {
                    break op;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("gate op submitted")
    }

    /// Slice 4: a gate is admitted only after a `candidate` settlement — on a failed or
    /// abandoned delivery no `#g1` is ever submitted. The admission decision is run by hand
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
            "no gate is admitted on a failed or abandoned delivery (slice 4)"
        );
    }

    /// Let a gate blocked on `flag` finish and wait for its Operation to settle.
    pub(super) async fn release_gate(
        &self,
        flag: &Path,
        attempt: &str,
    ) -> calm_server::operation::Operation {
        std::fs::write(flag, b"").unwrap();
        let op = self.wait_gate_op(attempt).await;
        tokio::time::timeout(WAIT, self.runtime.wait(&op.id))
            .await
            .expect("gate op terminal")
            .unwrap();
        self.runtime
            .find_by_kind_and_idempotency(TASK_VERIFY_KIND, &format!("{attempt}#g1"))
            .await
            .unwrap()
            .unwrap()
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

    pub(super) async fn set_track_lifecycle(&self, lifecycle: TrackLifecycle) {
        self.boot
            .repo
            .track_update(
                self.track(),
                calm_server::model::TrackPatch {
                    lifecycle: Some(lifecycle),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(self.track_lifecycle().await, lifecycle);
    }

    pub(super) async fn track_lifecycle(&self) -> TrackLifecycle {
        self.boot
            .repo
            .track_get(self.track())
            .await
            .unwrap()
            .unwrap()
            .lifecycle
    }

    /// A second Track in the same Area with its own Planner card: the caller identity of "another
    /// Track's Planner".
    pub(super) async fn other_track_planner(&self) -> ToolCallIdentity {
        let track = self
            .boot
            .repo
            .track_create(NewTrack {
                template_input: None,
                area_id: self.boot.area_id.clone(),
                title: "other track".into(),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let card_id = format!("planner-of-{}", track.id.as_str());
        // `card_create_with_id_tx` reads then writes: an immediate transaction, never a deferred
        // one that would upgrade under the read.
        let pool = self.pool();
        let mut tx = begin_immediate_tx(&pool).await.unwrap();
        card_create_with_id_tx(
            &mut tx,
            card_id.clone(),
            NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: Value::Null,
            },
            CardRole::Planner,
            true,
            &self.boot.card_role_cache,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        ToolCallIdentity {
            card_id,
            track_id: Some(track.id.as_str().to_string()),
            session_id: "other-planner-session".into(),
            thread_id: "other-planner-thread".into(),
            ..planner_identity(&self.boot)
        }
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
            daemon: SharedCodexAppServer::new_stub(self.boot.repo.clone()),
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
pub(super) struct AbandonmentRowView {
    pub(super) delivery_id: String,
    pub(super) producer_attempt_id: String,
    pub(super) request_idempotency_key: String,
    pub(super) reason: Option<String>,
    pub(super) task_outcome: String,
    pub(super) task_status: String,
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

pub(super) fn action_args(
    task: &Task,
    delivery_id: &str,
    idempotency_key: &str,
    action: &str,
    reason: Option<&str>,
) -> Value {
    let mut args = json!({
        "key": task.key, "expected_attempt_id": task.id, "expected_delivery_id": delivery_id,
        "idempotency_key": idempotency_key, "action": action,
    });
    if let Some(reason) = reason {
        args["reason"] = json!(reason);
    }
    args
}

/// A state refusal: `-32409` (the repository's Conflict code) and the `refused:` sentence
/// verbatim — no `task_delivery:` prefix (5.1.11).
pub(super) fn assert_refused(result: &Result<Value, RpcError>, needle: &str) {
    let error = match result {
        Err(error) => error,
        Ok(receipt) => panic!("expected a refusal containing {needle:?}, got {receipt}"),
    };
    assert_eq!(error.code, -32409, "{error:?}");
    assert!(error.message.starts_with("refused:"), "{error:?}");
    assert!(error.message.contains(needle), "{error:?}");
}

/// A malformed argument: `-32602`.
pub(super) fn assert_invalid_params(result: &Result<Value, RpcError>, needle: &str) {
    let error = match result {
        Err(error) => error,
        Ok(receipt) => panic!("expected -32602 containing {needle:?}, got {receipt}"),
    };
    assert_eq!(error.code, RpcError::INVALID_PARAMS, "{error:?}");
    assert!(error.message.contains(needle), "{error:?}");
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
    assert_eq!(committed[0]["branch"], fx.slice_branch(&worker.card_id));

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
    assert_eq!(
        entry["candidate"]["delivery"]["failure"]["retry_allowed"],
        true
    );
    let summary = fx.plan_summary_entry("crash-noref").await;
    assert_eq!(
        summary["candidate"]["delivery"]["failure"]["code"], "commit_failed",
        "{summary}"
    );
    assert_eq!(
        summary["candidate"]["delivery"]["failure"]["retry_allowed"],
        true
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
    }

    // The positive case: the slice branch delivers.
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
        let slice = fx.slice_branch(&worker.card_id);
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
// The workspace is gone: `workspace_missing`, never retryable, no retained path in the wake.
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
    // Slice 3: the decision clause offers `abandon` alone and quotes the row's delivery id.
    assert!(
        text.ends_with(&format!(
            "Decide: calm.task.delivery{{action:\"abandon\", expected_delivery_id:\"{}\"}}.",
            row.delivery_id
        )),
        "{text}"
    );
    assert!(!text.to_ascii_lowercase().contains("retry"), "{text}");
    let entry = fx.plan_entry("gone").await;
    assert_eq!(
        entry["candidate"]["delivery"]["failure"]["retry_allowed"], false,
        "{entry}"
    );
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
    let recovered = recover_harnesses_on_boot(
        fx.boot.repo.clone(),
        EventBus::new(),
        fx.boot.card_role_cache.clone(),
        areas,
        SharedCodexAppServer::new_fake_running_with_pending(fx.boot.repo.clone(), None),
        &registry,
        &calm_server::harness::new_track_delete_locks(),
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
// A26: a slice 1 lease (base recorded, no delivery policy) stays on today's path.
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
        let legacy_key = format!(
            "dev.neige.git-forge:{}:{}:git.commit:auto",
            fx.track(),
            worker.card_id
        );
        match provider {
            AgentProvider::Codex => {
                let op = tokio::time::timeout(WAIT, async {
                    loop {
                        if let Some(op) = fx.forge_op(&legacy_key).await {
                            break op;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .expect("Codex still auto-commits");
                fx.runtime.wait(&op.id).await.unwrap();
                let committed = fx.worktree_committed_events(&worker.card_id).await;
                assert_eq!(committed.len(), 1, "{name}");
                assert!(
                    committed[0].get("delivery_id").is_none(),
                    "{name}: {committed:?}"
                );
            }
            _ => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                assert!(
                    fx.forge_op(&legacy_key).await.is_none(),
                    "{name}: Claude does not commit"
                );
                assert_eq!(
                    git(&lease.path, &["rev-parse", "HEAD"]),
                    lease.base_sha,
                    "{name}"
                );
            }
        }
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

    // An isolated declaration (no dependencies allowed; nothing schedules it: no listener, no poke).
    declare(
        &fx.boot,
        json!({"key": "isolated", "kind": "codex", "goal": "isolated",
            "declared_by": PLANNER_DECLARATION_AUTHOR, "ready": true, "no_gate_reason": "fixture",
            "context": {"neige_execution": {"version": "isolated-codex-v1", "workspace": "empty"}}}),
    )
    .await;

    let list = call_tool(
        &fx.boot,
        "calm.plan.list",
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

    let isolated = entry("isolated");
    assert_eq!(
        isolated["candidate"],
        json!({"binding": "none", "reason": "isolated"}),
        "{isolated}"
    );

    for key in [
        "pending",
        "running",
        "spawn-failed",
        "timed-out",
        "done",
        "isolated",
    ] {
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
// Slice 3: `calm.task.delivery{retry|abandon}`.
// ===========================================================================

// ---------------------------------------------------------------------------
// A8a: a retry after a failed delivery mints the candidate; a candidate refuses a retry.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_after_failed_delivery_mints_candidate() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let (worker, task, lease) = fx.hook_failing_task("retry-ok", json!({})).await;
    let first_settled = fx.wait_settled(&task.id).await;
    failure_of(settled_result(&first_settled).0);
    let first = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(first.ordinal, 1);
    wait_observations(&planner, 1).await;

    remove_pre_commit(&lease);
    let receipt = fx
        .retry(&task, &first.delivery_id, "req-1", Some("hook removed"))
        .await
        .expect("retry admitted");
    let new_id = receipt["delivery_id"].as_str().unwrap().to_string();
    assert_ne!(new_id, first.delivery_id);
    // D2's retry receipt: the new delivery and the action, no task key (nothing about the tasks
    // row is persisted by a retry).
    assert_eq!(
        receipt,
        json!({"delivery_id": new_id, "ordinal": 2, "action": "retry"})
    );

    let second_settled = fx.wait_settled_nth(&task.id, 2).await;
    let (result, wake_reason) = settled_result(&second_settled);
    let (candidate_id, commit_sha, _, _) = candidate_of(result);
    assert_eq!(candidate_id, new_id);
    assert_eq!(wake_reason, DeliveryWakeReason::UngatedCandidate);
    let second = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(second.delivery_id, new_id);
    assert_eq!(second.ordinal, 2);
    assert_eq!(
        second.predecessor_delivery_id.as_deref(),
        Some(first.delivery_id.as_str())
    );
    assert_eq!(second.request_idempotency_key.as_deref(), Some("req-1"));
    assert_eq!(second.reason.as_deref(), Some("hook removed"));
    assert_eq!(second.settlement.as_deref(), Some("candidate"));
    // The first row keeps its failure.
    let first_now = fx.delivery_row_at(&task.id, 1).await.unwrap();
    assert_eq!(first_now.settlement.as_deref(), Some("failed"));
    let candidate = fx.candidate_row(&task.id).await.expect("candidate");
    assert_eq!(candidate.candidate_id, new_id);
    assert_eq!(candidate.commit_sha, commit_sha);
    assert_eq!(
        candidate.ref_name,
        format!(
            "refs/neige/candidates/{}/{}/{new_id}",
            fx.track(),
            worker.card_id
        )
    );
    assert_eq!(
        ref_target(&lease.git_common_dir, &candidate.ref_name).as_deref(),
        Some(commit_sha)
    );
    assert_eq!(fx.delivery_count(&task.id).await, 2);
    assert_eq!(fx.forge_op_count().await, 2);

    // Read surface: the latest row is what `delivery` shows, with its ordinal.
    let entry = fx.plan_entry("retry-ok").await;
    assert_eq!(
        entry["candidate"]["delivery"]["state"], "committed",
        "{entry}"
    );
    assert_eq!(entry["candidate"]["delivery"]["ordinal"], 2);
    assert_eq!(entry["candidate"]["delivery"]["delivery_id"], new_id);

    // Turns: the failed settlement, then the candidate settlement — one turn for the retry.
    let pending = wait_observations(&planner, 2).await;
    assert!(
        matches!(&pending[1], Observation::TaskGitDeliverySettled { result: DeliverySettlement::Candidate { .. }, delivery_id: Some(id), .. } if id == &new_id),
        "{pending:?}"
    );
    assert_observations_settle_at(&planner, 2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_refused_when_candidate_exists() {
    let fx = fixture().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("has-candidate", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "delivered\n").unwrap();
    fx.complete(&worker, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;
    candidate_of(settled_result(&settled).0);
    let first = fx.delivery_row(&task.id).await.unwrap();
    assert!(fx.candidate_row(&task.id).await.is_some());

    // A hand-made later delivery that failed: the latest row is `failed`, the candidate exists.
    sqlx::query(
        "INSERT INTO task_git_deliveries (delivery_id, track_id, producer_attempt_id, card_id, \
         lease_id, ordinal, operation_key, forge_idempotency_key, predecessor_delivery_id, \
         request_idempotency_key, reason, created_at_ms, settlement, settled_event_id, \
         failure_code, failure_reason, retry_allowed, wake_reason) \
         VALUES ('forged-2', ?1, ?2, ?3, ?4, 2, 'forged-key', 'forged-idem', ?5, NULL, NULL, ?6, \
         'failed', ?7, 'commit_failed', 'forged failure', 1, 'failed')",
    )
    .bind(fx.track())
    .bind(&task.id)
    .bind(&worker.card_id)
    .bind(&lease.lease_id)
    .bind(&first.delivery_id)
    .bind(now_ms())
    .bind(settled.id)
    .execute(&fx.pool())
    .await
    .unwrap();
    assert_eq!(fx.delivery_count(&task.id).await, 2);

    for (action, key) in [("retry", "req-r"), ("abandon", "req-a")] {
        let result = fx
            .delivery_action(action_args(&task, "forged-2", key, action, None))
            .await;
        assert_refused(&result, "refused: delivery forged-2 rows are inconsistent");
        assert!(
            result
                .as_ref()
                .unwrap_err()
                .message
                .contains("candidate_with_failed_settlement"),
            "{result:?}"
        );
    }
    assert_eq!(fx.delivery_count(&task.id).await, 2, "no ordinal 3");
    assert!(fx.abandonment_row("forged-2").await.is_none());
    assert_eq!(fx.forge_op_count().await, 1);
    // The plain candidate (no forged row) refuses with the candidate sentence.
    sqlx::query("DELETE FROM task_git_deliveries WHERE delivery_id = 'forged-2'")
        .execute(&fx.pool())
        .await
        .unwrap();
    let result = fx.retry(&task, &first.delivery_id, "req-c", None).await;
    assert_refused(
        &result,
        &format!(
            "refused: attempt already has candidate {}; accept it with calm.task.verdict",
            first.delivery_id
        ),
    );
    assert_eq!(fx.delivery_count(&task.id).await, 1);
}

// ---------------------------------------------------------------------------
// A8b / A8c: the expected delivery must be the latest; a pending delivery refuses.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_refused_when_expected_delivery_is_not_latest() {
    let fx = fixture().await;
    let (_, task, _) = fx.hook_failing_task("stale-expected", json!({})).await;
    fx.wait_settled(&task.id).await;
    let first = fx.delivery_row(&task.id).await.unwrap();
    // The hook stays: the retry fails too, leaving two failed deliveries.
    fx.retry(&task, &first.delivery_id, "req-1", None)
        .await
        .expect("first retry admitted");
    let second_settled = fx.wait_settled_nth(&task.id, 2).await;
    failure_of(settled_result(&second_settled).0);
    let second = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(second.ordinal, 2);
    assert_eq!(second.settlement.as_deref(), Some("failed"));

    let result = fx.retry(&task, &first.delivery_id, "req-2", None).await;
    assert_refused(
        &result,
        &format!(
            "refused: expected_delivery_id {} is not the latest delivery of attempt {} (latest: {})",
            first.delivery_id, task.id, second.delivery_id
        ),
    );
    let result = fx.abandon(&task, &first.delivery_id, "req-3", None).await;
    assert_refused(&result, &second.delivery_id);
    assert_eq!(fx.delivery_count(&task.id).await, 2, "no ordinal 3");
    assert!(fx.abandonment_row(&first.delivery_id).await.is_none());
    assert!(fx.abandonment_row(&second.delivery_id).await.is_none());

    // A stale attempt id is refused too, naming the current one.
    let mut stale = task.clone();
    stale.id = "not-the-current-attempt".into();
    let result = fx.retry(&stale, &second.delivery_id, "req-4", None).await;
    assert_refused(
        &result,
        &format!(
            "refused: expected_attempt_id not-the-current-attempt is not the current attempt of task stale-expected (current: {})",
            task.id
        ),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_refused_while_delivery_pending() {
    let fx = fixture().await;
    // No listener, no submission: the row exists and nothing will ever settle it here.
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("still-pending", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "pending\n").unwrap();
    fx.report_only(&worker, &task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert!(row.settlement.is_none());
    assert!(lease.path.is_dir(), "every other admission fact holds");

    let result = fx.retry(&task, &row.delivery_id, "req-1", None).await;
    assert_refused(
        &result,
        &format!(
            "refused: delivery {} is pending; wait for task.git_delivery_settled",
            row.delivery_id
        ),
    );
    let result = fx.abandon(&task, &row.delivery_id, "req-2", None).await;
    assert_refused(&result, "is pending");
    assert_eq!(fx.delivery_count(&task.id).await, 1, "no ordinal 2");
    assert_eq!(fx.forge_op_count().await, 0);
    assert!(fx.abandonment_row(&row.delivery_id).await.is_none());
}

// ---------------------------------------------------------------------------
// Retry-only refusals: `retry_allowed = 0` (the row, not the directory) and a missing workspace.
// ---------------------------------------------------------------------------

/// A `workspace_missing` settlement (`retry_allowed = 0`); the lease directory is RE-CREATED
/// before the retry so the directory check cannot stand in for the row check.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_refused_when_not_retryable() {
    let mut fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("not-retryable", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "gone\n").unwrap();
    fx.report_only(&worker, &task.id).await;
    std::fs::remove_dir_all(&lease.path).unwrap();
    fx.reboot().await;
    let settled = fx.wait_settled(&task.id).await;
    let (code, _, retry_allowed) = failure_of(settled_result(&settled).0);
    assert_eq!(code, DeliveryFailureCode::WorkspaceMissing);
    assert!(!retry_allowed);
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(row.retry_allowed, Some(0));
    let ops_before = fx.forge_op_count().await;

    std::fs::create_dir_all(&lease.path).unwrap();
    assert!(lease.path.is_dir(), "the directory check passes again");
    let result = fx.retry(&task, &row.delivery_id, "req-1", None).await;
    assert_refused(
        &result,
        &format!(
            "refused: delivery {} is not retryable (workspace_missing)",
            row.delivery_id
        ),
    );
    assert_eq!(fx.delivery_count(&task.id).await, 1, "no ordinal 2");
    assert_eq!(fx.forge_op_count().await, ops_before);
    // The way out is offered and still open.
    let receipt = fx
        .abandon(&task, &row.delivery_id, "req-2", None)
        .await
        .expect("abandon admitted");
    assert_eq!(receipt["action"], "abandon", "{receipt}");
}

/// A retryable failure (`commit_failed`, `retry_allowed = 1`) whose workspace was removed after
/// the settlement: refused by the directory check, no row, no Operation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_refused_when_workspace_is_gone() {
    let fx = fixture().await;
    let (_, task, lease) = fx.hook_failing_task("workspace-gone", json!({})).await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(row.failure_code.as_deref(), Some("commit_failed"));
    assert_eq!(row.retry_allowed, Some(1));
    let ops_before = fx.forge_op_count().await;

    std::fs::remove_dir_all(&lease.path).unwrap();
    let result = fx.retry(&task, &row.delivery_id, "req-1", None).await;
    assert_refused(
        &result,
        &format!(
            "refused: delivery {} cannot be retried, workspace {} is missing",
            row.delivery_id,
            lease.path.display()
        ),
    );
    assert!(
        fx.delivery_row_at(&task.id, 2).await.is_none(),
        "no ordinal 2"
    );
    assert_eq!(fx.delivery_count(&task.id).await, 1);
    assert_eq!(fx.forge_op_count().await, ops_before);
}

// ---------------------------------------------------------------------------
// Wire codes: malformed arguments are `-32602` and commit nothing; refusals are `-32409`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delivery_action_rejects_malformed_arguments() {
    let fx = fixture().await;
    let (_, task, _) = fx.hook_failing_task("malformed", json!({})).await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    let ops_before = fx.forge_op_count().await;

    // A present `reason` that is not a string.
    let mut args = action_args(&task, &row.delivery_id, "req-1", "abandon", None);
    args["reason"] = json!(123);
    let result = fx.delivery_action(args).await;
    assert_invalid_params(&result, "`reason` must be a string");
    assert!(
        fx.abandonment_row(&row.delivery_id).await.is_none(),
        "nothing committed"
    );
    assert_eq!(fx.task_columns(&task.id).await.status, TaskStatus::Done);

    // An unknown action; a missing and an empty required string.
    let result = fx
        .delivery_action(action_args(&task, &row.delivery_id, "req-2", "skip", None))
        .await;
    assert_invalid_params(&result, "unknown action `skip`");
    let mut args = action_args(&task, &row.delivery_id, "req-3", "retry", None);
    args.as_object_mut().unwrap().remove("expected_delivery_id");
    let result = fx.delivery_action(args).await;
    assert_invalid_params(&result, "missing `expected_delivery_id`");
    let mut args = action_args(&task, &row.delivery_id, "req-4", "retry", None);
    args["idempotency_key"] = json!("  ");
    let result = fx.delivery_action(args).await;
    assert_invalid_params(&result, "missing `idempotency_key`");
    assert_eq!(fx.delivery_count(&task.id).await, 1);
    assert_eq!(fx.forge_op_count().await, ops_before);
    for key in ["req-1", "req-2", "req-3", "req-4"] {
        let used: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM task_git_deliveries WHERE request_idempotency_key = ?1",
        )
        .bind(key)
        .fetch_one(&fx.pool())
        .await
        .unwrap();
        assert_eq!(used, 0, "{key} was never persisted");
    }

    // A state refusal, for contrast: `-32409` and the sentence verbatim.
    let result = fx.retry(&task, "not-the-latest", "req-5", None).await;
    assert_refused(&result, "refused: expected_delivery_id not-the-latest");
    assert!(
        !result.unwrap_err().message.contains("task_delivery:"),
        "no handler prefix on a refusal"
    );
}

// ---------------------------------------------------------------------------
// No delivery row: the refusal names why (legacy lease, not reported, ended without delivery).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_delivery_row_refusals_name_the_reason() {
    let fx = fixture().await;
    fx.dispatcher.abort_event_listener_for_test();

    // A legacy lease (slice 1: base recorded, no delivery policy): no kernel delivery exists.
    let legacy = fx.new_worker("legacy", AgentProvider::Codex).await;
    let lease = fx.kernel_lease(&legacy.card_id).await;
    sqlx::query("UPDATE workspace_leases SET delivery_policy = NULL WHERE lease_id = ?1")
        .bind(&lease.lease_id)
        .execute(&fx.pool())
        .await
        .unwrap();
    let task = fx
        .running_task("legacy", "codex", &legacy.card_id, json!({}))
        .await;
    let result = fx.retry(&task, "none", "req-1", None).await;
    assert_refused(
        &result,
        &format!(
            "refused: attempt {} has a legacy lease (no kernel delivery); nothing to retry or abandon",
            task.id
        ),
    );

    // A kernel lease whose worker is still running: no report yet.
    let worker = fx.codex_worker();
    fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("unreported", "codex", &worker.card_id, json!({}))
        .await;
    let result = fx.abandon(&task, "none", "req-2", None).await;
    assert_refused(
        &result,
        &format!(
            "refused: attempt {} has not reported yet; wait for the worker's report",
            task.id
        ),
    );

    // The same attempt ended `failed` without ever reporting (a worker timeout).
    sqlx::query(
        "UPDATE tasks SET status = 'failed', status_detail = 'worker-timeout', \
         finished_at_ms = ?2, updated_at_ms = ?2 WHERE id = ?1",
    )
    .bind(&task.id)
    .bind(now_ms())
    .execute(&fx.pool())
    .await
    .unwrap();
    let result = fx.retry(&task, "none", "req-3", None).await;
    assert_refused(
        &result,
        &format!(
            "refused: attempt {} ended without a delivery (worker-timeout); declare a new task",
            task.id
        ),
    );
    assert_eq!(fx.delivery_count(&task.id).await, 0);
    assert_eq!(fx.forge_op_count().await, 0);
}

// ---------------------------------------------------------------------------
// A8d: replay first.
// ---------------------------------------------------------------------------

/// A gated task: the gate finishes (the row flips `verifying → done`) between the first call and
/// the replay, and the replay is the original receipt whole — a retry receipt carries nothing
/// observed at call time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_replay_after_success_returns_original_receipt() {
    let fx = fixture().await;
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let (_, task, lease) = fx
        .hook_failing_task(
            "replay",
            json!({"gate": gate_waiting_for(&flag), "no_gate_reason": null}),
        )
        .await;
    fx.wait_settled(&task.id).await;
    assert_eq!(
        fx.task_columns(&task.id).await.status,
        TaskStatus::Verifying
    );
    let first = fx.delivery_row(&task.id).await.unwrap();
    remove_pre_commit(&lease);
    let receipt = fx
        .retry(&task, &first.delivery_id, "req-1", Some("again"))
        .await
        .unwrap();
    assert_eq!(
        receipt,
        json!({"delivery_id": receipt["delivery_id"], "ordinal": 2, "action": "retry"}),
        "the retry receipt has no task key"
    );
    let settled = fx.wait_settled_nth(&task.id, 2).await;
    candidate_of(settled_result(&settled).0);
    assert!(fx.candidate_row(&task.id).await.is_some());

    // The gate finishes between the two calls: the row is `done` now, the state `committed`.
    fx.release_gate(&flag, &task.id).await;
    wait_gate_result(&fx, &task.id).await;
    assert_eq!(fx.task_columns(&task.id).await.status, TaskStatus::Done);

    // The same key with the same fingerprint replays anyway, and the receipt is identical.
    let replay = fx
        .retry(&task, &first.delivery_id, "req-1", Some("again"))
        .await
        .expect("replay is not admission");
    assert_eq!(replay, receipt, "the original receipt, whole");
    assert_eq!(fx.delivery_count(&task.id).await, 2, "no third row");
    assert_eq!(fx.forge_op_count().await, 2);
    assert_eq!(fx.settled_events_for(&task.id).await.len(), 2);
}

/// The replay key is Track-scoped: another Track's Planner quoting this Track's attempt, request
/// key and fingerprint is refused (no current attempt in its Track), never handed this Track's
/// receipt — for a retry and for an abandon.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delivery_action_replay_is_track_scoped() {
    let fx = fixture().await;
    let other_planner = fx.other_track_planner().await;
    let (_, task, lease) = fx.hook_failing_task("scoped", json!({})).await;
    fx.wait_settled(&task.id).await;
    let first = fx.delivery_row(&task.id).await.unwrap();
    remove_pre_commit(&lease);
    let receipt = fx
        .retry(&task, &first.delivery_id, "req-1", Some("mine"))
        .await
        .unwrap();
    fx.wait_settled_nth(&task.id, 2).await;
    let refused_sentence =
        "refused: task scoped has no current attempt in this Track; declare a new task";

    // Track B's Planner, Track A's attempt + key + fingerprint: not A's receipt.
    let foreign = call_tool(
        &fx.boot,
        TOOL_TASK_DELIVERY,
        other_planner.clone(),
        action_args(&task, &first.delivery_id, "req-1", "retry", Some("mine")),
    )
    .await;
    assert_refused(&foreign, refused_sentence);
    assert_eq!(fx.delivery_count(&task.id).await, 2, "no third row");
    // A's own replay still answers.
    assert_eq!(
        fx.retry(&task, &first.delivery_id, "req-1", Some("mine"))
            .await
            .unwrap(),
        receipt
    );

    // The abandonment key on another fixture, the same way.
    let fx = fixture().await;
    let other_planner = fx.other_track_planner().await;
    let (_, task, _) = fx.hook_failing_task("scoped", json!({})).await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    let receipt = fx
        .abandon(&task, &row.delivery_id, "req-a", Some("mine"))
        .await
        .unwrap();
    assert_eq!(receipt["task_outcome"], "done_unchanged", "{receipt}");
    let foreign = call_tool(
        &fx.boot,
        TOOL_TASK_DELIVERY,
        other_planner,
        action_args(&task, &row.delivery_id, "req-a", "abandon", Some("mine")),
    )
    .await;
    assert_refused(&foreign, refused_sentence);
    assert_eq!(
        fx.abandon(&task, &row.delivery_id, "req-a", Some("mine"))
            .await
            .unwrap(),
        receipt
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_replay_same_key_different_fingerprint_conflicts() {
    let fx = fixture().await;
    let (_, task, lease) = fx.hook_failing_task("fingerprint", json!({})).await;
    fx.wait_settled(&task.id).await;
    let first = fx.delivery_row(&task.id).await.unwrap();
    remove_pre_commit(&lease);
    fx.retry(&task, &first.delivery_id, "req-1", Some("fix"))
        .await
        .unwrap();
    fx.wait_settled_nth(&task.id, 2).await;

    // Same key, different reason / different expected delivery / different action.
    let second = fx.delivery_row(&task.id).await.unwrap();
    for args in [
        action_args(&task, &first.delivery_id, "req-1", "retry", Some("other")),
        action_args(&task, &first.delivery_id, "req-1", "retry", None),
        action_args(&task, &second.delivery_id, "req-1", "retry", Some("fix")),
        action_args(&task, &first.delivery_id, "req-1", "abandon", Some("fix")),
    ] {
        let result = fx.delivery_action(args.clone()).await;
        assert_refused(
            &result,
            &format!(
                "refused: idempotency_key req-1 was already used for attempt {}",
                task.id
            ),
        );
        let _ = args;
    }
    assert_eq!(fx.delivery_count(&task.id).await, 2);
    assert!(fx.abandonment_row(&first.delivery_id).await.is_none());
    assert!(fx.abandonment_row(&second.delivery_id).await.is_none());
}

// ---------------------------------------------------------------------------
// A9: abandon releases the Track budget; the row flips through its own guard; no wake.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandon_frees_track_budget() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let (_, first_task, _) = fx
        .hook_failing_task(
            "first",
            json!({"gate": gate_waiting_for(&flag), "no_gate_reason": null}),
        )
        .await;
    assert_eq!(
        current(&fx.boot, "first").await.status,
        TaskStatus::Verifying
    );
    let settled = fx.wait_settled(&first_task.id).await;
    let (code, _, retry_allowed) = failure_of(settled_result(&settled).0);
    assert_eq!(code, DeliveryFailureCode::CommitFailed);
    assert!(retry_allowed);
    let row = fx.delivery_row(&first_task.id).await.unwrap();
    wait_observations(&planner, 1).await;
    fx.assert_no_gate_op(&first_task.id).await;

    // The default budget is 1: a second ready task stays pending behind the `verifying` row.
    declare(
        &fx.boot,
        json!({"key": "second", "kind": "codex", "goal": "next", "declared_by": PLANNER_DECLARATION_AUTHOR,
            "ready": true, "no_gate_reason": "budget fixture"}),
    )
    .await;
    fx.scheduler()
        .schedule_track(fx.boot.track_id.clone())
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        current(&fx.boot, "second").await.status,
        TaskStatus::Pending,
        "budget 1 is held by the gated row"
    );

    let task_failed_handled = fx.arm_task_failed_barrier(&first_task.id);
    let receipt = fx
        .abandon(&first_task, &row.delivery_id, "req-a", Some("hook rejects"))
        .await
        .expect("abandon admitted");
    assert_eq!(
        receipt,
        json!({
            "delivery_id": row.delivery_id, "ordinal": 1, "action": "abandon",
            "task_outcome": "failed", "task_status": "failed",
        })
    );
    // The budget slot is free: the second task is claimed (the spawn itself has no adapter here).
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

    let columns = fx.task_columns(&first_task.id).await;
    assert_eq!(columns.status, TaskStatus::Failed);
    assert_eq!(columns.status_detail.as_deref(), Some("delivery-abandoned"));
    assert!(columns.finished_at_ms.is_some());
    let failed = fx.events_for(TASK_FAILED_KIND, &first_task.id).await;
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
        matches!(&failed[0].event, Event::TaskFailed { reason, .. } if reason == "delivery-abandoned: hook rejects"),
        "{:?}",
        failed[0].event
    );
    assert_eq!(
        fx.abandonment_row(&row.delivery_id).await,
        Some(AbandonmentRowView {
            delivery_id: row.delivery_id.clone(),
            producer_attempt_id: first_task.id.clone(),
            request_idempotency_key: "req-a".into(),
            reason: Some("hook rejects".into()),
            task_outcome: "failed".into(),
            task_status: "failed".into(),
        })
    );
    assert_eq!(
        fx.delivery_row(&first_task.id)
            .await
            .unwrap()
            .settlement
            .as_deref(),
        Some("failed"),
        "the settlement is not touched"
    );

    // Zero turns for the abandon: the tool receipt is the answer. The Dispatcher has handled the
    // `task.failed` (barrier) and the harness ingress is drained before the count is read.
    wait_task_failed_handled(&task_failed_handled).await;
    let pending = assert_observations_exactly(&planner, 1).await;
    assert!(
        matches!(&pending[0], Observation::TaskGitDeliverySettled { .. }),
        "{pending:?}"
    );
    // No gate was ever admitted for the failed delivery, so none reports on the row.
    fx.assert_no_gate_op(&first_task.id).await;
    assert!(
        fx.events_for(GATE_RESULT_KIND, &first_task.id)
            .await
            .is_empty()
    );
    assert_eq!(
        fx.task_columns(&first_task.id)
            .await
            .status_detail
            .as_deref(),
        Some("delivery-abandoned")
    );
    // Nothing was emitted since the barrier (the discarded verdict appended no event, asserted
    // above): the drained count is the whole answer.
    assert_observations_exactly(&planner, 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandon_flips_verifying_row() {
    let fx = fixture().await;
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let (_, task, _) = fx
        .hook_failing_task(
            "flip",
            json!({"gate": gate_waiting_for(&flag), "no_gate_reason": null}),
        )
        .await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(
        fx.task_columns(&task.id).await.status,
        TaskStatus::Verifying
    );

    let receipt = fx
        .abandon(&task, &row.delivery_id, "req-a", Some("give up"))
        .await
        .expect("the verifying row admits the abandon");
    assert_eq!(receipt["task_outcome"], "failed", "{receipt}");
    assert_eq!(receipt["task_status"], "failed");
    let columns = fx.task_columns(&task.id).await;
    assert_eq!(columns.status, TaskStatus::Failed, "{columns:?}");
    assert_eq!(columns.status_detail.as_deref(), Some("delivery-abandoned"));
    assert_eq!(fx.events_for(TASK_FAILED_KIND, &task.id).await.len(), 1);
    let abandonment = fx.abandonment_row(&row.delivery_id).await.unwrap();
    assert_eq!(abandonment.task_outcome, "failed");
    assert_eq!(abandonment.task_status, "failed");

    // Replay: the same key and fingerprint returns the same receipt and writes nothing more.
    let replay = fx
        .abandon(&task, &row.delivery_id, "req-a", Some("give up"))
        .await
        .unwrap();
    assert_eq!(replay, receipt);
    assert_eq!(fx.events_for(TASK_FAILED_KIND, &task.id).await.len(), 1);
    // A different fingerprint under the same key, and a new key on the abandoned delivery.
    let result = fx
        .abandon(&task, &row.delivery_id, "req-a", Some("other"))
        .await;
    assert_refused(&result, "refused: idempotency_key req-a was already used");
    let result = fx.abandon(&task, &row.delivery_id, "req-b", None).await;
    assert_refused(
        &result,
        &format!("refused: delivery {} was abandoned", row.delivery_id),
    );
    let result = fx.retry(&task, &row.delivery_id, "req-c", None).await;
    assert_refused(&result, "was abandoned");
    assert_eq!(fx.delivery_count(&task.id).await, 1);

    // Read surface.
    let entry = fx.plan_entry("flip").await;
    assert_eq!(
        entry["candidate"]["delivery"],
        json!({
            "state": "abandoned", "delivery_id": row.delivery_id, "ordinal": 1,
            "reason": "give up", "task_outcome": "failed", "task_status": "failed",
        }),
        "{entry}"
    );
    fx.assert_no_gate_op(&task.id).await;
}

// ---------------------------------------------------------------------------
// Abandon promotes the Track like every other terminal flip: `working → reviewing` in the same
// transaction, its events broadcast behind the `task.failed`; not for `done_unchanged`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandon_promotes_working_track_to_reviewing() {
    let fx = fixture().await;
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let (_, task, _) = fx
        .hook_failing_task(
            "promote",
            json!({"gate": gate_waiting_for(&flag), "no_gate_reason": null}),
        )
        .await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    fx.set_track_lifecycle(TrackLifecycle::Working).await;
    let mut bus = fx.boot.ctx.events.subscribe();

    let receipt = fx
        .abandon(&task, &row.delivery_id, "req-a", Some("give up"))
        .await
        .expect("admitted");
    assert_eq!(receipt["task_outcome"], "failed", "{receipt}");
    assert_eq!(fx.track_lifecycle().await, TrackLifecycle::Reviewing);

    // The three events of the one transaction, in append order, each broadcast with its id.
    let persisted = fx
        .boot
        .repo
        .events_for_track(
            fx.track(),
            &[TASK_FAILED_KIND, "track.lifecycle_changed", "track.updated"],
            None,
        )
        .await
        .unwrap();
    let kinds: Vec<&str> = persisted.iter().map(|row| row.event.kind_tag()).collect();
    assert_eq!(
        kinds,
        vec![TASK_FAILED_KIND, "track.lifecycle_changed", "track.updated"],
        "{persisted:?}"
    );
    assert!(
        matches!(&persisted[1].event, Event::TrackLifecycleChanged { from: TrackLifecycle::Working, to: TrackLifecycle::Reviewing, agent_message: Some(message), .. } if message == "[auto] delivery abandoned"),
        "{:?}",
        persisted[1].event
    );
    let mut broadcast = Vec::new();
    while broadcast.len() < 3 {
        let envelope = tokio::time::timeout(WAIT, bus.recv())
            .await
            .expect("the abandon's events are broadcast")
            .unwrap();
        if [TASK_FAILED_KIND, "track.lifecycle_changed", "track.updated"]
            .contains(&envelope.event.kind_tag())
        {
            broadcast.push((envelope.id, envelope.event.kind_tag()));
        }
    }
    assert_eq!(
        broadcast,
        persisted
            .iter()
            .map(|row| (row.id, row.event.kind_tag()))
            .collect::<Vec<_>>(),
        "broadcast in append order under the persisted ids"
    );
    fx.assert_no_gate_op(&task.id).await;

    // `done_unchanged` (an ungated `done` row) is no terminal flip: the Track stays `working`.
    let fx = fixture().await;
    let (_, task, _) = fx.hook_failing_task("no-promote", json!({})).await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    fx.set_track_lifecycle(TrackLifecycle::Working).await;
    let receipt = fx
        .abandon(&task, &row.delivery_id, "req-a", None)
        .await
        .unwrap();
    assert_eq!(receipt["task_outcome"], "done_unchanged", "{receipt}");
    assert_eq!(fx.track_lifecycle().await, TrackLifecycle::Working);
    assert!(
        fx.boot
            .repo
            .events_for_track(fx.track(), &["track.lifecycle_changed"], None)
            .await
            .unwrap()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// A9b: the gate flipped the row first — `already_terminal`, nothing written to the row.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandon_on_terminal_row_reports_already_terminal() {
    for (name, gate_cmd, expected_status) in [
        ("gate-passed", "true", TaskStatus::Done),
        ("gate-failed", "exit 1", TaskStatus::Failed),
    ] {
        let fx = fixture().await;
        let planner = fx.planner().await;
        let worker = fx.codex_worker();
        let lease = fx.kernel_lease(&worker.card_id).await;
        // The commit blocks in the hook until the gate has flipped the row, then fails.
        let flag = fx.track_root.parent().unwrap().join("commit-may-fail");
        install_pre_commit(&lease, &hook_waiting_for(&flag, 1));
        let task = fx
            .running_task(
                name,
                "codex",
                &worker.card_id,
                json!({"gate": {"steps": [{"name": "g", "cmd": gate_cmd}]}, "no_gate_reason": null}),
            )
            .await;
        std::fs::write(lease.path.join("worker.txt"), "gated\n").unwrap();
        fx.complete(&worker, &task.id).await;
        // Slice 4 admits no gate while the delivery is pending (the decision is pinned by
        // `gate_is_not_submitted_while_delivery_pending`; this is a state read, the live drive
        // holds `gate:<task>` while it waits on the held delivery): the terminal row beside a
        // pending delivery is the D12 (i) window, played by hand with the verdict a slice 2/3
        // gate would have recorded.
        assert!(
            fx.runtime
                .find_by_kind_and_idempotency(TASK_VERIFY_KIND, &format!("{}#g1", task.id))
                .await
                .unwrap()
                .is_none()
        );
        sqlx::query(
            "UPDATE tasks SET status = ?1, status_detail = ?2, gate_attempt = 1, \
             gate_result_json = ?3, finished_at_ms = ?4 WHERE id = ?5",
        )
        .bind(expected_status)
        .bind((expected_status == TaskStatus::Failed).then_some("gate-red"))
        .bind(
            json!({"passed": expected_status == TaskStatus::Done, "log_tail": "", "log_path": "/l", "attempt": 1})
                .to_string(),
        )
        .bind(now_ms())
        .bind(&task.id)
        .execute(&fx.pool())
        .await
        .unwrap();
        assert_eq!(
            current(&fx.boot, name).await.status,
            expected_status,
            "{name}"
        );
        assert!(fx.settled_events_for(&task.id).await.is_empty(), "{name}");
        std::fs::write(&flag, b"").unwrap();
        let settled = fx.wait_settled(&task.id).await;
        failure_of(settled_result(&settled).0);
        let row = fx.delivery_row(&task.id).await.unwrap();
        let before = fx.task_columns(&task.id).await;
        assert_eq!(before.status, expected_status);
        // One turn so far: the failed settlement (the hand-flipped verdict pushed nothing).
        wait_observations(&planner, 1).await;

        let receipt = fx
            .abandon(&task, &row.delivery_id, "req-a", Some("late"))
            .await
            .expect("admitted on a terminal row");
        assert_eq!(
            receipt["task_outcome"], "already_terminal",
            "{name}: {receipt}"
        );
        assert_eq!(
            receipt["task_status"],
            json!(expected_status),
            "{name}: {receipt}"
        );
        assert_eq!(receipt["delivery_id"], row.delivery_id);
        let abandonment = fx.abandonment_row(&row.delivery_id).await.unwrap();
        assert_eq!(abandonment.task_outcome, "already_terminal", "{name}");
        assert_eq!(
            abandonment.task_status,
            json!(expected_status).as_str().unwrap(),
            "{name}"
        );
        assert_eq!(
            fx.task_columns(&task.id).await,
            before,
            "{name}: the row is not touched"
        );
        assert!(
            fx.events_for(TASK_FAILED_KIND, &task.id).await.is_empty(),
            "{name}: no task.failed"
        );
        assert!(fx.events_for(GATE_RESULT_KIND, &task.id).await.is_empty());
        // The replay is the persisted row's receipt, whole.
        assert_eq!(
            fx.abandon(&task, &row.delivery_id, "req-a", Some("late"))
                .await
                .unwrap(),
            receipt,
            "{name}"
        );
        // An `already_terminal` abandon appends no event at all (asserted above), so there is no
        // envelope to barrier on: the one handled turn (the failed settlement) is the count and
        // this is the timing check that nothing else arrives.
        assert_observations_settle_at(&planner, 1).await;
        let entry = fx.plan_entry(name).await;
        assert_eq!(
            entry["candidate"]["delivery"]["state"], "abandoned",
            "{name}: {entry}"
        );
        assert_eq!(
            entry["candidate"]["delivery"]["task_outcome"],
            "already_terminal"
        );
    }

    // The positive case: a row still `verifying` reads `failed`.
    let fx = fixture().await;
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let (_, task, _) = fx
        .hook_failing_task(
            "still-verifying",
            json!({"gate": gate_waiting_for(&flag), "no_gate_reason": null}),
        )
        .await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    let receipt = fx
        .abandon(&task, &row.delivery_id, "req-a", None)
        .await
        .unwrap();
    assert_eq!(receipt["task_outcome"], "failed", "{receipt}");
    assert_eq!(receipt["task_status"], "failed");
    assert_eq!(fx.task_columns(&task.id).await.status, TaskStatus::Failed);
    fx.assert_no_gate_op(&task.id).await;
}

// ---------------------------------------------------------------------------
// A9c: abandon clears the gate-process triple. Since slice 4 no gate runs beside a failed
// delivery (admission waits for a candidate), so the triple a slice 2/3 build recorded for a
// parked gate is set by hand — the D12 (i) window — and the abandon must still clear it.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandon_clears_gate_pid_triple() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
    let (_, task, _) = fx
        .hook_failing_task(
            "pid-triple",
            json!({"gate": gate_waiting_for(&flag), "no_gate_reason": null}),
        )
        .await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    // No gate is admitted on the failed delivery; the triple of a pre-slice-4 parked gate is
    // recorded by hand.
    fx.assert_no_gate_op(&task.id).await;
    sqlx::query(
        "UPDATE tasks SET gate_pid = 4242, gate_pid_starttime = 1, gate_pid_boot_id = 'stale-boot' WHERE id = ?1",
    )
    .bind(&task.id)
    .execute(&fx.pool())
    .await
    .unwrap();
    let before = fx.task_columns(&task.id).await;
    assert_eq!(before.status, TaskStatus::Verifying);
    assert!(before.gate_pid_starttime.is_some() && before.gate_pid_boot_id.is_some());
    assert!(before.finished_at_ms.is_none());
    wait_observations(&planner, 1).await;

    let task_failed_handled = fx.arm_task_failed_barrier(&task.id);
    fx.abandon(&task, &row.delivery_id, "req-a", None)
        .await
        .expect("admitted");
    let after = fx.task_columns(&task.id).await;
    assert_eq!(after.status, TaskStatus::Failed, "{after:?}");
    assert_eq!(after.status_detail.as_deref(), Some("delivery-abandoned"));
    assert_eq!(after.gate_pid, None, "{after:?}");
    assert_eq!(after.gate_pid_starttime, None, "{after:?}");
    assert_eq!(after.gate_pid_boot_id, None, "{after:?}");
    assert!(after.finished_at_ms.is_some(), "{after:?}");

    // Still no gate, and no gate result on the abandoned row.
    fx.assert_no_gate_op(&task.id).await;
    assert!(
        fx.events_for(GATE_RESULT_KIND, &task.id).await.is_empty(),
        "no task.gate_result"
    );
    assert_eq!(
        fx.task_columns(&task.id).await,
        after,
        "the row is unchanged"
    );
    // Zero turns: the Dispatcher handled the abandon's `task.failed` (barrier), the discarded
    // verdict appended nothing (asserted above), and the drained ingress holds the one settlement.
    wait_task_failed_handled(&task_failed_handled).await;
    assert_observations_exactly(&planner, 1).await;

    // The gate-flip path clears the same triple (A9b's `done` fixture, on the same fixture).
    let fx = fixture().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task(
            "gate-clears",
            "codex",
            &worker.card_id,
            json!({"gate": {"steps": [{"name": "g", "cmd": "true"}]}, "no_gate_reason": null}),
        )
        .await;
    std::fs::write(lease.path.join("worker.txt"), "gated\n").unwrap();
    fx.complete(&worker, &task.id).await;
    wait_gate_result(&fx, &task.id).await;
    let columns = fx.task_columns(&task.id).await;
    assert_eq!(columns.status, TaskStatus::Done);
    assert!(
        columns.gate_pid.is_none()
            && columns.gate_pid_starttime.is_none()
            && columns.gate_pid_boot_id.is_none(),
        "{columns:?}"
    );
}

// ---------------------------------------------------------------------------
// Retry admission reads the delivery row, not the result file.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_admission_reads_delivery_row_not_result_file() {
    let fx = fixture().await;
    let (_, task, lease) = fx.hook_failing_task("no-result-file", json!({})).await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(row.retry_allowed, Some(1));
    // The wrapper's files are gone before the retry is asked for.
    let op = fx.wait_forge_op(&task.id).await;
    let result_path = PathBuf::from(op.payload["result_path"].as_str().unwrap());
    for suffix in ["", ".code", ".stdout"] {
        let mut path = result_path.clone().into_os_string();
        path.push(suffix);
        let _ = std::fs::remove_file(path);
    }
    assert_eq!(result_code(&fx, &task.id).await, None);
    // And the settlement event is gone too: only the row can say `retry_allowed`.
    sqlx::query("DELETE FROM events WHERE kind = ?1")
        .bind(SETTLED_KIND)
        .execute(&fx.pool())
        .await
        .unwrap();
    assert!(fx.settled_events_for(&task.id).await.is_empty());

    remove_pre_commit(&lease);
    let receipt = fx
        .retry(&task, &row.delivery_id, "req-1", None)
        .await
        .expect("admitted from the row alone");
    assert_eq!(receipt["ordinal"], 2, "{receipt}");
    let settled = fx.wait_settled(&task.id).await;
    candidate_of(settled_result(&settled).0);
    assert!(fx.candidate_row(&task.id).await.is_some());
}

// ---------------------------------------------------------------------------
// A9d / A6c: Track and Area deletion after an abandonment; candidate refs go with the Track.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn track_and_area_delete_after_abandonment() {
    for delete_area in [false, true] {
        let fx = fixture().await;
        let flag = fx.track_root.parent().unwrap().join("gate-may-finish");
        let (_, task, lease) = fx
            .hook_failing_task(
                "abandon-then-delete",
                json!({"gate": gate_waiting_for(&flag), "no_gate_reason": null}),
            )
            .await;
        fx.wait_settled(&task.id).await;
        let row = fx.delivery_row(&task.id).await.unwrap();
        fx.abandon(&task, &row.delivery_id, "req-a", Some("bye"))
            .await
            .unwrap();
        assert!(fx.abandonment_row(&row.delivery_id).await.is_some());
        fx.assert_no_gate_op(&task.id).await;
        // A second worker delivers a candidate on the same Track: its ref must go with the Track.
        remove_pre_commit(&lease);
        let other = fx.new_worker("delivers", AgentProvider::Codex).await;
        let other_lease = fx.kernel_lease(&other.card_id).await;
        let other_task = fx
            .running_task("delivers", "codex", &other.card_id, json!({}))
            .await;
        std::fs::write(other_lease.path.join("worker.txt"), "ok\n").unwrap();
        fx.complete(&other, &other_task.id).await;
        candidate_of(settled_result(&fx.wait_settled(&other_task.id).await).0);
        assert_eq!(candidate_refs(&lease.git_common_dir, fx.track()).len(), 1);
        for table in [
            "task_git_deliveries",
            "task_git_delivery_abandonments",
            "task_candidates",
        ] {
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
        // (`task_git_candidate_decisions` is slice 6; the three slice-2/3 tables and the leases.)
        for table in [
            "task_git_deliveries",
            "task_git_delivery_abandonments",
            "task_candidates",
            "workspace_leases",
        ] {
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

// ---------------------------------------------------------------------------
// A25b positive: `git merge --abort`, then a retry succeeds.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_after_merge_abort_succeeds() {
    let fx = fixture().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("merge-abort", "codex", &worker.card_id, json!({}))
        .await;
    let slice = fx.slice_branch(&worker.card_id);
    git(&lease.path, &["checkout", "-q", "-b", "other"]);
    commit_file(&lease.path, "README.md", "other\n", "other");
    git(&lease.path, &["checkout", "-q", &slice]);
    let mine = commit_file(&lease.path, "README.md", "mine\n", "mine");
    assert!(
        !git_output(&lease.path, &["merge", "--no-commit", "other"])
            .status
            .success()
    );
    fx.complete(&worker, &task.id).await;
    let settled = fx.wait_settled(&task.id).await;
    let (code, reason, retry_allowed) = failure_of(settled_result(&settled).0);
    assert_eq!(code, DeliveryFailureCode::ProvenanceMismatch);
    assert!(reason.starts_with(&failure_sentence("15")), "{reason}");
    assert!(retry_allowed);
    let first = fx.delivery_row(&task.id).await.unwrap();

    // The Planner (through a terminal task, in production) aborts the merge, then retries.
    git(&lease.path, &["merge", "--abort"]);
    assert_eq!(git(&lease.path, &["rev-parse", "HEAD"]), mine);
    assert_eq!(git(&lease.path, &["status", "--porcelain"]), "");
    let receipt = fx
        .retry(&task, &first.delivery_id, "req-1", Some("merge aborted"))
        .await
        .expect("admitted");
    let settled = fx.wait_settled_nth(&task.id, 2).await;
    let (candidate_id, commit_sha, _, base_is_ancestor) = candidate_of(settled_result(&settled).0);
    assert_eq!(candidate_id, receipt["delivery_id"]);
    assert_eq!(commit_sha, mine, "the branch tip as it stands now");
    assert!(base_is_ancestor);
    let candidate = fx.candidate_row(&task.id).await.unwrap();
    assert_eq!(candidate.commit_sha, mine);
    assert_eq!(
        ref_target(&lease.git_common_dir, &candidate.ref_name).as_deref(),
        Some(mine.as_str())
    );
}

// ---------------------------------------------------------------------------
// A23b first fixture (slice 4 shape): the delivery fails, the retry succeeds, and only then is
// the gate admitted — two turns over the whole lifecycle: the `failed` settlement and the
// gate result; the retry's `candidate` settlement is `deferred_to_gate` and silent.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_then_gate_wakes_twice() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let flag = fx.track_root.parent().unwrap().join("commit-may-fail");
    install_pre_commit(&lease, &hook_waiting_for(&flag, 1));
    let task = fx
        .running_task(
            "gate-then-retry",
            "codex",
            &worker.card_id,
            json!({"gate": {"steps": [{"name": "t", "cmd": "true"}]}, "no_gate_reason": null}),
        )
        .await;
    std::fs::write(lease.path.join("worker.txt"), "gated\n").unwrap();

    fx.complete(&worker, &task.id).await;
    // No gate while the delivery is held: admission waits for settlement (the decision is
    // pinned by `gate_is_not_submitted_while_delivery_pending`; here the harness ingress is
    // drained and the absence read).
    assert_observations_exactly(&planner, 0).await;
    assert!(
        fx.runtime
            .find_by_kind_and_idempotency(TASK_VERIFY_KIND, &format!("{}#g1", task.id))
            .await
            .unwrap()
            .is_none()
    );
    // Turn 1: the delivery fails; the gate is not admitted on a failed delivery.
    std::fs::write(&flag, b"").unwrap();
    let failed = fx.wait_settled(&task.id).await;
    failure_of(settled_result(&failed).0);
    let first = fx.delivery_row(&task.id).await.unwrap();
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
    assert_eq!(
        current(&fx.boot, "gate-then-retry").await.status,
        TaskStatus::Verifying
    );
    assert!(
        fx.runtime
            .find_by_kind_and_idempotency(TASK_VERIFY_KIND, &format!("{}#g1", task.id))
            .await
            .unwrap()
            .is_none(),
        "no gate on a failed delivery"
    );
    // The retry settles as a candidate on the still-verifying row: deferred to the gate, silent.
    remove_pre_commit(&lease);
    let receipt = fx
        .retry(&task, &first.delivery_id, "req-1", None)
        .await
        .expect("admitted");
    assert_eq!(
        receipt,
        json!({"delivery_id": receipt["delivery_id"], "ordinal": 2, "action": "retry"}),
        "{receipt}"
    );
    let settled = fx.wait_settled_nth(&task.id, 2).await;
    let (result, wake_reason) = settled_result(&settled);
    let (candidate_id, ..) = candidate_of(result);
    assert_eq!(wake_reason, DeliveryWakeReason::DeferredToGate);
    assert_eq!(
        fx.delivery_row(&task.id)
            .await
            .unwrap()
            .wake_reason
            .as_deref(),
        Some("deferred_to_gate")
    );
    // Turn 2: the gate, admitted after the retry's candidate, verifies that candidate.
    let gate = wait_gate_result(&fx, &task.id).await;
    let Event::TaskGateResult { passed, target, .. } = &gate.event else {
        panic!("{gate:?}");
    };
    assert!(passed);
    assert!(
        matches!(target, Some(calm_types::verify_target::VerifyTarget::Candidate { candidate_id: id, .. }) if id == candidate_id),
        "{target:?}"
    );
    let pending = wait_observations(&planner, 2).await;
    assert!(
        matches!(
            &pending[1],
            Observation::TaskGateResult { passed: true, .. }
        ),
        "{pending:?}"
    );
    // The gate result is handled (its observation is the second turn); the drained ingress
    // holds exactly the two.
    assert_observations_exactly(&planner, 2).await;
    assert_eq!(fx.events_for(GATE_RESULT_KIND, &task.id).await.len(), 1);
    assert_eq!(
        current(&fx.boot, "gate-then-retry").await.status,
        TaskStatus::Done
    );
}

// ---------------------------------------------------------------------------
// After a retry the latest row is `ordinal 2`: a repeated `task.complete` resubmits it under
// its own key and the runtime dedups.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_completion_after_retry_resubmits_latest_row_idempotently() {
    let fx = fixture().await;
    let (worker, task, lease) = fx.hook_failing_task("repeat-complete", json!({})).await;
    fx.wait_settled(&task.id).await;
    let first = fx.delivery_row(&task.id).await.unwrap();
    remove_pre_commit(&lease);
    fx.retry(&task, &first.delivery_id, "req-1", None)
        .await
        .unwrap();
    fx.wait_settled_nth(&task.id, 2).await;
    let second = fx.delivery_row(&task.id).await.unwrap();
    assert_eq!(second.ordinal, 2);
    assert_eq!(fx.forge_op_count().await, 2);

    // REPEATED: the handler reads the latest row (ordinal 2) and resubmits under its key.
    fx.complete(&worker, &task.id).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        fx.forge_op_count().await,
        2,
        "the runtime dedups the same key"
    );
    assert_eq!(fx.delivery_count(&task.id).await, 2);
    assert_eq!(fx.settled_events_for(&task.id).await.len(), 2);
    let (key, idem): (String, String) = sqlx::query_as(
        "SELECT operation_key, idempotency_key FROM operations WHERE kind = ?1 ORDER BY created_at_ms DESC, id DESC LIMIT 1",
    )
    .bind(FORGE_ACTION_KIND)
    .fetch_one(&fx.pool())
    .await
    .unwrap();
    assert_eq!(key, second.operation_key);
    assert_eq!(idem, second.forge_idempotency_key);
    assert_eq!(fx.delivery_row(&task.id).await.unwrap(), second);
}

// ---------------------------------------------------------------------------
// The failed wake names the decision; the read surface reads the abandonment.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_wake_text_names_the_delivery_action() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let (_, task, _) = fx.hook_failing_task("wake-decide", json!({})).await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    let pending = wait_observations(&planner, 1).await;
    let Observation::TaskGitDeliverySettled { delivery_id, .. } = &pending[0] else {
        panic!("{pending:?}");
    };
    assert_eq!(delivery_id.as_deref(), Some(row.delivery_id.as_str()));
    let text = pending[0].to_turn_text();
    assert!(text.contains("Decide: calm.task.delivery{"), "{text}");
    assert!(text.contains("action:\"retry\"|\"abandon\""), "{text}");
    assert!(
        text.contains(&format!("expected_delivery_id:\"{}\"", row.delivery_id)),
        "{text}"
    );
    assert!(
        text.contains(&format!("runs/{}.md. Decide:", task.id)),
        "the decision follows the worker-output pointer: {text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plan_list_reads_abandoned_delivery() {
    let fx = fixture().await;
    let (_, task, _) = fx.hook_failing_task("read-abandoned", json!({})).await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    let before = fx.plan_entry("read-abandoned").await;
    assert_eq!(
        before["candidate"]["delivery"]["state"], "failed",
        "{before}"
    );
    assert_eq!(before["candidate"]["delivery"]["ordinal"], 1);
    let before_summary = fx.plan_summary_entry("read-abandoned").await;
    assert_eq!(
        before_summary["candidate"]["delivery"]["ordinal"], 1,
        "{before_summary}"
    );

    // Ungated: the `done` row is left alone.
    let receipt = fx
        .abandon(&task, &row.delivery_id, "req-a", Some("not worth it"))
        .await
        .unwrap();
    assert_eq!(
        receipt,
        json!({
            "delivery_id": row.delivery_id, "ordinal": 1, "action": "abandon",
            "task_outcome": "done_unchanged", "task_status": "done",
        })
    );
    assert_eq!(
        current(&fx.boot, "read-abandoned").await.status,
        TaskStatus::Done
    );
    assert!(fx.events_for(TASK_FAILED_KIND, &task.id).await.is_empty());
    let entry = fx.plan_entry("read-abandoned").await;
    assert_eq!(entry["candidate"]["binding"], "bound", "{entry}");
    assert_eq!(
        entry["candidate"]["delivery"],
        json!({
            "state": "abandoned", "delivery_id": row.delivery_id, "ordinal": 1,
            "reason": "not worth it", "task_outcome": "done_unchanged", "task_status": "done",
        }),
        "{entry}"
    );
    let summary = fx.plan_summary_entry("read-abandoned").await;
    assert_eq!(
        summary["candidate"]["delivery"],
        json!({
            "state": "abandoned", "delivery_id": row.delivery_id, "ordinal": 1,
            "task_outcome": "done_unchanged", "task_status": "done",
        }),
        "the summary keeps the decision facts, not the reason: {summary}"
    );
    // Without a reason the key is absent, not null.
    let fx = fixture().await;
    let (_, task, _) = fx.hook_failing_task("no-reason", json!({})).await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    fx.abandon(&task, &row.delivery_id, "req-a", None)
        .await
        .unwrap();
    let entry = fx.plan_entry("no-reason").await;
    assert!(
        entry["candidate"]["delivery"].get("reason").is_none(),
        "{entry}"
    );
    assert_eq!(entry["candidate"]["delivery"]["state"], "abandoned");
}

// ---------------------------------------------------------------------------
// G11: the Track lifecycle rule is `calm.task.verdict`'s — a Done Track admits both.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delivery_action_on_done_track_follows_verdict_rule() {
    let fx = fixture().await;
    let (_, task, lease) = fx.hook_failing_task("done-track", json!({})).await;
    fx.wait_settled(&task.id).await;
    let row = fx.delivery_row(&task.id).await.unwrap();
    fx.boot
        .repo
        .track_update(
            fx.track(),
            calm_server::model::TrackPatch {
                lifecycle: Some(TrackLifecycle::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fx.boot
            .repo
            .track_get(fx.track())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        TrackLifecycle::Done
    );

    // The rule as verified: `calm.task.verdict` consults no lifecycle on a Done Track.
    call_tool(
        &fx.boot,
        "calm.task.verdict",
        planner_identity(&fx.boot),
        json!({"idempotency_key": task.id, "status": "rejected", "reason": "late", "message": "verdict on a Done Track"}),
    )
    .await
    .expect("calm.task.verdict is admitted on a Done Track");
    // So is `calm.task.delivery`, both actions.
    remove_pre_commit(&lease);
    let receipt = fx
        .retry(&task, &row.delivery_id, "req-1", None)
        .await
        .expect("retry admitted on a Done Track");
    assert_eq!(receipt["ordinal"], 2, "{receipt}");
    let settled = fx.wait_settled_nth(&task.id, 2).await;
    candidate_of(settled_result(&settled).0);
    let fx2 = fixture().await;
    let (_, task2, _) = fx2.hook_failing_task("done-track-abandon", json!({})).await;
    fx2.wait_settled(&task2.id).await;
    let row2 = fx2.delivery_row(&task2.id).await.unwrap();
    fx2.boot
        .repo
        .track_update(
            fx2.track(),
            calm_server::model::TrackPatch {
                lifecycle: Some(TrackLifecycle::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let receipt = fx2
        .abandon(&task2, &row2.delivery_id, "req-1", None)
        .await
        .expect("abandon admitted on a Done Track");
    assert_eq!(receipt["task_outcome"], "done_unchanged", "{receipt}");
    assert_eq!(
        fx2.boot
            .repo
            .track_get(fx2.track())
            .await
            .unwrap()
            .unwrap()
            .lifecycle,
        TrackLifecycle::Done,
        "the action does not move the lifecycle"
    );
}
