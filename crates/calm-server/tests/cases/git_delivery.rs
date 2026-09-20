//! #1727 S4 slice 2 PR-B: kernel git delivery for attached workers, wired end to end — the
//! report transaction's delivery row, the keyed forge submission, the scheduler's settlement
//! into `task_candidates` + `task.git_delivery_settled`, the deferred self-report and the
//! `calm.plan.list.candidate` read surface. Design §6 rows A3–A7, A23–A23c, A25–A31.
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
use calm_server::dispatcher::Dispatcher;
use calm_server::event::{Event, EventBus};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, Observation, PlannerHarness,
    PlannerHarnessParams, recover_harnesses_on_boot,
};
use calm_server::mcp_server::registry::ToolCallIdentity;
use calm_server::model::{CardRole, NewCard, Task, TaskStatus, now_ms};
use calm_server::operation::forge_action_adapter::{FORGE_ACTION_KIND, ForgeActionAdapter};
use calm_server::operation::task_verify_adapter::TaskVerifyAdapter;
use calm_server::operation::{
    OperationCompletionBus, OperationRuntime, ProviderAdapter, SpawnCtx, SqlxOperationRepo,
};
use calm_server::scheduler::Scheduler;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{CodexClient, DaemonClient};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::test_seams::{KernelWorkspaceLease, take_kernel_workspace_lease_for_test};
use calm_types::git_candidate::{DeliveryFailureCode, DeliverySettlement, DeliveryWakeReason};
use calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR;
use serde_json::{Value, json};

const SETTLED_KIND: &str = "task.git_delivery_settled";
const WAIT: Duration = Duration::from_secs(30);
/// The fixed failure sentences the settlement writes (`prompts/delivery/git-delivery-failures.md`).
const FAILURE_SENTENCES: &str = include_str!("../../prompts/delivery/git-delivery-failures.md");

fn failure_sentence(key: &str) -> String {
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

fn git_output(dir: &Path, args: &[&str]) -> std::process::Output {
    calm_server::test_seams::neige_git_command_for_test()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git")
}

fn git(dir: &Path, args: &[&str]) -> String {
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

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.email", "delivery@example.test"]);
    git(path, &["config", "user.name", "Delivery Test"]);
    std::fs::write(path.join("README.md"), "initial\n").unwrap();
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-q", "-m", "initial"]);
}

fn commit_file(dir: &Path, name: &str, content: &str, message: &str) -> String {
    std::fs::write(dir.join(name), content).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// `git --git-dir=<common_dir> rev-parse --verify <ref>^{commit}` — the settlement's own check.
fn ref_target(common_dir: &Path, ref_name: &str) -> Option<String> {
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

fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

// ---------------------------------------------------------------------------
// The fixture: the MCP boot, a git repository as the Track workspace, a runtime with the forge
// and task-verify adapters, a Dispatcher whose live listener pushes the Planner harness.
// ---------------------------------------------------------------------------

struct Fx {
    boot: Boot,
    runtime: Arc<OperationRuntime>,
    dispatcher: Dispatcher,
    harness: HarnessRegistry,
    /// The Track workspace (the main repository, or a linked worktree of it).
    track_root: PathBuf,
    workspace_root: PathBuf,
    _tmp: tempfile::TempDir,
}

/// The delivery row as the tests read it: identity plus the six settlement columns.
#[derive(Debug, Clone, sqlx::FromRow)]
struct DeliveryRowView {
    delivery_id: String,
    ordinal: i64,
    operation_key: String,
    forge_idempotency_key: String,
    settlement: Option<String>,
    settled_event_id: Option<i64>,
    failure_code: Option<String>,
    failure_reason: Option<String>,
    retry_allowed: Option<i64>,
    wake_reason: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct CandidateRowView {
    candidate_id: String,
    commit_sha: String,
    base_sha: String,
    base_is_ancestor: i64,
    ref_name: String,
    git_common_dir: String,
}

async fn fixture() -> Fx {
    fixture_with(|tmp| {
        let repo = tmp.join("repo");
        init_repo(&repo);
        repo
    })
    .await
}

/// `track_root` builds the Track workspace under the temp dir and returns it.
async fn fixture_with(track_root: impl FnOnce(&Path) -> PathBuf) -> Fx {
    let boot = boot().await;
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
    let runtime = Arc::new(OperationRuntime::new_unchecked(
        operation_repo,
        vec![
            Arc::new(ForgeActionAdapter::new()) as Arc<dyn ProviderAdapter>,
            Arc::new(TaskVerifyAdapter::new(gate_logs_dir.clone())) as Arc<dyn ProviderAdapter>,
        ],
        events.clone(),
        completion,
        spawn_ctx,
    ));
    assert!(boot.ctx.operation_runtime.set(runtime.clone()).is_ok());
    let harness = HarnessRegistry::new();
    let dispatcher = spawn_dispatcher(&boot, &runtime, &harness, terminal_renderer, daemon);
    Fx {
        boot,
        runtime,
        dispatcher,
        harness,
        track_root,
        workspace_root,
        _tmp: tmp,
    }
}

fn spawn_dispatcher(
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
    fn pool(&self) -> sqlx::SqlitePool {
        self.boot.repo.sqlite_pool().unwrap()
    }

    fn track(&self) -> &str {
        self.boot.track_id.as_str()
    }

    fn scheduler(&self) -> Arc<Scheduler> {
        self.dispatcher.scheduler()
    }

    /// A new Dispatcher (live listener + fresh scheduler) over the same runtime, events bus and
    /// harness registry — no recovery, no boot sweep: the live path resumes where
    /// `abort_event_listener_for_test` stopped it.
    fn respawn_dispatcher(&mut self) {
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
    }

    /// A kernel restart: a new Dispatcher, then operation recovery and the boot sweep.
    async fn reboot(&mut self) {
        self.respawn_dispatcher();
        let plan = self.runtime.recover_on_boot().await.unwrap();
        self.runtime.apply_recovery(plan).await.unwrap();
        let scheduler = self.scheduler();
        scheduler.mark_context_sweep_boot_complete();
        scheduler.sweep_boot().await;
    }

    /// The `Boot`'s worker card as a Codex worker.
    fn codex_worker(&self) -> ToolCallIdentity {
        worker_identity(&self.boot)
    }

    /// The `Boot`'s worker card as a Claude worker (the identity's provider is what the emit
    /// handler reads; `call_tool` bypasses the transport hop that derives it).
    fn claude_worker(&self) -> ToolCallIdentity {
        ToolCallIdentity {
            provider: AgentProvider::Claude,
            ..worker_identity(&self.boot)
        }
    }

    /// Another worker card on the same Track with its own live session.
    async fn new_worker(&self, name: &str, provider: AgentProvider) -> ToolCallIdentity {
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
    async fn running_task(&self, key: &str, kind: &str, worker: &str, extra: Value) -> Task {
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

    async fn claim_running(&self, task_id: &str, worker: &str) {
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
    /// HEAD base, the kernel-policy row, the worktree pinned to the base.
    async fn kernel_lease(&self, card: &str) -> KernelWorkspaceLease {
        take_kernel_workspace_lease_for_test(&self.pool(), self.track(), card, &self.workspace_root)
            .await
            .unwrap()
    }

    fn slice_branch(&self, card: &str) -> String {
        format!("neige/{}/{card}", self.track())
    }

    async fn complete(&self, worker: &ToolCallIdentity, task_id: &str) {
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
    async fn report_only(&self, worker: &ToolCallIdentity, task_id: &str) {
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

    async fn delivery_row(&self, attempt: &str) -> Option<DeliveryRowView> {
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

    async fn delivery_count(&self, attempt: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM task_git_deliveries WHERE producer_attempt_id = ?1",
        )
        .bind(attempt)
        .fetch_one(&self.pool())
        .await
        .unwrap()
    }

    async fn candidate_row(&self, attempt: &str) -> Option<CandidateRowView> {
        sqlx::query_as(
            "SELECT candidate_id, commit_sha, base_sha, base_is_ancestor, ref_name, git_common_dir \
             FROM task_candidates WHERE producer_attempt_id = ?1",
        )
        .bind(attempt)
        .fetch_optional(&self.pool())
        .await
        .unwrap()
    }

    async fn forge_op_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE kind = ?1")
            .bind(FORGE_ACTION_KIND)
            .fetch_one(&self.pool())
            .await
            .unwrap()
    }

    async fn forge_op(
        &self,
        forge_idempotency_key: &str,
    ) -> Option<calm_server::operation::Operation> {
        self.runtime
            .find_by_kind_and_idempotency(FORGE_ACTION_KIND, forge_idempotency_key)
            .await
            .unwrap()
    }

    /// Wait for the attempt's forge Operation to exist and reach a terminal phase.
    async fn wait_forge_op(&self, attempt: &str) -> calm_server::operation::Operation {
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
    async fn settled_events(&self) -> Vec<calm_server::db::TrackEvent> {
        self.boot
            .repo
            .events_for_track(self.track(), &[SETTLED_KIND], None)
            .await
            .unwrap()
    }

    async fn settled_events_for(&self, attempt: &str) -> Vec<calm_server::db::TrackEvent> {
        self.settled_events()
            .await
            .into_iter()
            .filter(|row| matches!(&row.event, Event::TaskGitDeliverySettled { task_id, .. } if task_id == attempt))
            .collect()
    }

    /// Wait for the attempt's settlement event and return it.
    async fn wait_settled(&self, attempt: &str) -> calm_server::db::TrackEvent {
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

    fn debug_state(&self) -> String {
        format!("track {} at {}", self.track(), self.track_root.display())
    }

    async fn worktree_committed_events(&self, card: &str) -> Vec<Value> {
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
    async fn plan_entry(&self, key: &str) -> Value {
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

    async fn plan_summary_entry(&self, key: &str) -> Value {
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

    /// A live Planner harness for this Track, registered where the Dispatcher pushes.
    async fn planner(&self) -> PlannerHarness {
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

async fn observations(handle: &PlannerHarness) -> Vec<Observation> {
    handle.snapshot().await.pending_observations().to_vec()
}

/// Wait until the harness holds `n` pending observations (or fail with what it holds).
async fn wait_observations(handle: &PlannerHarness, n: usize) -> Vec<Observation> {
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

/// Settle for a moment and assert the harness holds exactly `expected` observations.
async fn assert_observations_settle_at(
    handle: &PlannerHarness,
    expected: usize,
) -> Vec<Observation> {
    tokio::time::sleep(Duration::from_millis(400)).await;
    let pending = observations(handle).await;
    assert_eq!(pending.len(), expected, "{pending:?}");
    pending
}

fn settled_result(row: &calm_server::db::TrackEvent) -> (&DeliverySettlement, DeliveryWakeReason) {
    match &row.event {
        Event::TaskGitDeliverySettled {
            result,
            wake_reason,
            ..
        } => (result, *wake_reason),
        other => panic!("not a settlement: {other:?}"),
    }
}

fn candidate_of(result: &DeliverySettlement) -> (&str, &str, &str, bool) {
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

fn failure_of(result: &DeliverySettlement) -> (DeliveryFailureCode, &str, bool) {
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
async fn result_code(fx: &Fx, attempt: &str) -> Option<i32> {
    let op = fx.wait_forge_op(attempt).await;
    let result_path = PathBuf::from(op.payload["result_path"].as_str().unwrap());
    let mut code_path = result_path.into_os_string();
    code_path.push(".code");
    std::fs::read_to_string(code_path)
        .ok()
        .map(|text| text.trim().parse().unwrap())
}

// ---------------------------------------------------------------------------
// A3 / A3b / A7: a Claude worker's completion becomes a kernel candidate.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_worker_completion_yields_kernel_candidate() {
    let mut fx = fixture().await;
    let planner = fx.planner().await;
    let worker = fx.claude_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let task = fx
        .running_task("deliver", "claude", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "delivered\n").unwrap();

    // The report handler's own submission, observed before any scheduler pass could submit the
    // row under the same key (a pass would hide a handler that skips Claude). The Dispatcher's
    // live listener pokes the scheduler on `task.completed`, so it is stopped for this window;
    // the declaration's `plan.updated` poke is drained first (`schedule_track` waits for the
    // Track lock) and the poke count is held constant across the call.
    fx.dispatcher.abort_event_listener_for_test();
    let scheduler = fx.scheduler();
    scheduler.schedule_track(fx.boot.track_id.clone()).await;
    let pokes = scheduler.poke_count_for_test();
    fx.complete(&worker, &task.id).await;
    let row = fx.delivery_row(&task.id).await.expect("delivery row");
    assert_eq!(row.ordinal, 1);
    assert!(
        row.settlement.is_none(),
        "no scheduler pass has run: {row:?}"
    );
    assert_eq!(
        scheduler.poke_count_for_test(),
        pokes,
        "no scheduler poke in the window"
    );
    let op = fx
        .forge_op(&row.forge_idempotency_key)
        .await
        .expect("the report handler submitted the delivery before returning");
    assert_eq!(op.operation_key, row.operation_key, "under the row's key");
    assert_eq!(fx.forge_op_count().await, 1);

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
async fn park_without_result_file(fx: &Fx, attempt: &str) -> PathBuf {
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

async fn crashed_before_ref(fx: &mut Fx) -> (ToolCallIdentity, Task, KernelWorkspaceLease) {
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

async fn failing_hook_delivery(fx: &Fx) -> (ToolCallIdentity, Task) {
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    let hooks = lease.git_common_dir.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    write_executable(&hooks.join("pre-commit"), "#!/bin/sh\nexit 1\n");
    let task = fx
        .running_task("hook-red", "codex", &worker.card_id, json!({}))
        .await;
    std::fs::write(lease.path.join("worker.txt"), "rejected\n").unwrap();
    fx.complete(&worker, &task.id).await;
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
fn linked_worktree_track(tmp: &Path) -> PathBuf {
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
fn gate_waiting_for(flag: &Path) -> Value {
    json!({"steps": [{"name": "wait", "cmd": format!("until [ -f '{}' ]; do sleep 0.1; done", flag.display())}], "timeout_secs": 60})
}

async fn wait_gate_result(fx: &Fx, attempt: &str) -> calm_server::db::TrackEvent {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn candidate_settlement_wakes_when_gate_already_flipped() {
    let fx = fixture().await;
    let planner = fx.planner().await;
    let worker = fx.codex_worker();
    let lease = fx.kernel_lease(&worker.card_id).await;
    // The delivery's commit blocks in a pre-commit hook until the flag appears; the gate (`true`)
    // finishes first and flips the row to `done`.
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
    let gate = wait_gate_result(&fx, &task.id).await;
    assert!(matches!(
        &gate.event,
        Event::TaskGateResult { passed: true, .. }
    ));
    let pending = wait_observations(&planner, 1).await;
    assert!(
        matches!(
            &pending[0],
            Observation::TaskGateResult { passed: true, .. }
        ),
        "{pending:?}"
    );
    assert_eq!(
        current(&fx.boot, "gate-first").await.status,
        TaskStatus::Done
    );
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

    // The whole lifecycle: `task.gate_result`, then the `gate_already_terminal` settlement.
    let pending = wait_observations(&planner, 2).await;
    assert!(
        matches!(&pending[0], Observation::TaskGateResult { .. }),
        "{pending:?}"
    );
    assert!(
        matches!(
            &pending[1],
            Observation::TaskGitDeliverySettled {
                result: DeliverySettlement::Candidate { .. },
                ..
            }
        ),
        "{pending:?}"
    );
    assert_observations_settle_at(&planner, 2).await;
    assert_eq!(
        current(&fx.boot, "gate-first").await.status,
        TaskStatus::Done
    );
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
