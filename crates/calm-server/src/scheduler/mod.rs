//! Kernel task scheduler (issue #644 PR-B, design §5).
//!
//! Owned by the dispatcher construction site (same process, same
//! `Weak<OperationRuntime>` discipline). The scheduler is the only
//! component that moves plan tasks `pending → dispatched → running`;
//! worker reports move them onward inside the `calm.task.complete` /
//! `calm.task.fail` emit tx (`mcp_server::tools::emit`), and terminal
//! exits go through [`TerminalTaskHook`] / the sweep's running-terminal
//! arm — both share [`complete_terminal_task`].
//!
//! ## Policy-free guarantee (§5.4)
//!
//! The scheduler never re-runs a `failed` task, never reorders beyond
//! `(priority DESC, created_at ASC, key ASC)`, never edits the plan,
//! and never garbage-collects. Retry is the spec inserting a new task.
//! The only runtime judgment it holds is the persisted agent-worker
//! liveness deadline; terminal workers remain mechanically reconciled
//! by exit status.
//!
//! ## Triggers (§5.1)
//!
//! Envelopes (`plan.updated`, `wave.lifecycle_changed`,
//! `task.completed`, `task.failed`) poke [`Scheduler::poke`] from the
//! dispatcher's subscription loop. The bus is lossy, so liveness is
//! backstopped by [`Scheduler::sweep_all`] — run at boot (after
//! operation recovery), on `RecvError::Lagged`, and on a slow periodic
//! reconcile tick (`NEIGE_SCHEDULER_RECONCILE_SECS`, default 300).
//! Every sweep arm is guarded and idempotent, so a sweep racing live
//! handling is a no-op. The tick/Lagged backstops are boot-gated:
//! [`Scheduler::sweep_all`] no-ops until the boot funnel's
//! [`Scheduler::sweep_boot`] completes, preserving the documented
//! recovery → scheduler-sweep boot order.
//!
//! ## Single-winner claim (§5.4/§5.5)
//!
//! Per-wave mutex + dirty flag serialize scheduling passes; the claim
//! UPDATE (`WHERE status = 'pending'`) is the single-winner primitive;
//! the operations `(kind, idempotency_key)` unique index is the final
//! backstop. `Event::TaskDispatched` is appended IN the claim tx so
//! projections stay purely event-sourced (§5.6).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use serde_json::{Value, json};
use tokio::sync::{Notify, Semaphore};

use crate::db::sqlite::{
    SuccessReportFlip, TaskReporter, begin_immediate_tx, task_claim_pending_tx,
    task_fail_from_worker_tx, task_get_tx, task_mark_running_tx, task_mark_sub_wave_running_tx,
    task_report_success_from_worker_tx, task_stamp_missing_running_deadline_tx, tasks_by_wave_tx,
    wave_lifecycle_and_budget_tx,
};
use crate::db::{Repo, write_with_actor_events_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::{ActorId, WaveId};
use crate::model::{Task, TaskKind, TaskStatus, Wave, WaveLifecycle, new_id, now_ms};
use crate::operation::child_wave_adapter::{CHILD_WAVE_KIND, ChildWaveOperationPayload};
use crate::operation::claude_adapter::ClaudeWorkerOperationPayload;
use crate::operation::codex_adapter::CodexWorkerOperationPayload;
use crate::operation::spec_harness_start_adapter::SpecHarnessStartOperationPayload;
use crate::operation::task_verify_adapter::{
    GateResultCtx, GateVerdict, TASK_VERIFY_KIND, TaskVerifyOperationPayload,
    apply_gate_result_in_tx, gate_attempt_key,
};
use crate::operation::terminal_adapter::TerminalWorkerOperationPayload;
use crate::operation::workspace_lease::release_workspace_lease_for_card_repo;
use crate::operation::{OperationKey, OperationOutcome, OperationRuntime, Tx};
use crate::routes::terminal_cards::stable_payload_hash;
use crate::state::WriteContext;
use crate::task_context::{ContextMetrics, TaskContextMonitor, context_ref};
use crate::wave_lifecycle::auto_transition_if_current_in_tx;

/// Kernel default per-wave task budget when `waves.task_budget` is NULL
/// and `NEIGE_WAVE_TASK_BUDGET` is unset/invalid. **1 is deliberate**
/// (§5.3): workers and gates share one directory tree today (no
/// worktrees, risk R2); >1 is opt-in per wave.
pub const DEFAULT_WAVE_TASK_BUDGET: i64 = 1;

/// Default reconcile-tick period (§5.1 liveness backstop).
pub const DEFAULT_RECONCILE_SECS: u64 = 300;

/// Default wall-clock window for agent workers to report task
/// completion/failure after the running stamp.
pub const DEFAULT_TASK_RUN_TIMEOUT_SECS: u64 = 7200;

/// Internal sentinel: a guarded flip affected 0 rows because another
/// writer (claim race, fast worker report, earlier sweep) won. Carried
/// through `CalmError::Conflict` so the eventized-write helper rolls
/// the tx back without persisting events; callers translate it back
/// into a silent no-op.
const RACE_LOST: &str = "scheduler: race lost (guarded write no-op)";

pub(crate) fn race_lost_err() -> CalmError {
    CalmError::Conflict(RACE_LOST.into())
}

pub(crate) fn is_race_lost(e: &CalmError) -> bool {
    matches!(e, CalmError::Conflict(m) if m == RACE_LOST)
}

fn fence_revision_matches(current: Option<Option<i64>>, frozen: u64) -> bool {
    current
        .flatten()
        .and_then(|value| u64::try_from(value).ok())
        == Some(frozen)
}

async fn mark_running_timeout_cleanup_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
    task_id: &str,
    now: i64,
) -> Result<u64> {
    let marker = serde_json::to_string(&json!({
        "task_id": task_id,
        "requested_at_ms": now,
        "reason": "running_liveness_timeout",
    }))?;
    let rows = sqlx::query(
        r#"UPDATE worker_sessions
           SET handle_state_json = json_set(
                 COALESCE(handle_state_json, '{}'),
                 '$.timeout_cleanup',
                 json(?1)
               ),
               updated_at_ms = ?2
           WHERE card_id = ?3
             AND state IN ('starting','running','idle','turn_pending')"#,
    )
    .bind(marker)
    .bind(now)
    .bind(card_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(rows)
}

/// §5.2 lifecycle gating: schedule only while the wave is in an active
/// lifecycle. `Draft` (user hasn't kicked off), `Blocked` (needs user),
/// and the terminal states hold *new* claims; in-flight tasks are
/// unaffected (no interruption — out of scope).
pub fn lifecycle_allows_scheduling(lifecycle: WaveLifecycle) -> bool {
    matches!(
        lifecycle,
        WaveLifecycle::Planning
            | WaveLifecycle::Dispatching
            | WaveLifecycle::Working
            | WaveLifecycle::Reviewing
    )
}

/// §5.2 ready-set computation over one wave's plan rows (already in
/// scheduler order: `priority DESC, created_at_ms ASC, key ASC`).
///
/// `running_cost` counts `dispatched`/`running`/`verifying` —
/// `verifying` deliberately occupies budget (gates are heavy and share
/// the checkout; future-proofed here even though no task reaches
/// `verifying` before PR-C). Deps are satisfied **only** by `done`
/// siblings: `canceled`/`failed` never satisfy a dependency (§3.1), so
/// successors sit `pending` until the spec revises the plan.
///
/// Issue #760 slice 1: resource disjointness for `budget > 1` is not a
/// second scheduler predicate. Codex tasks acquire a durable workspace
/// lease at operation claim time, and the lease path is
/// `.claude/worktrees/<wave>/<card>`, so concurrent claims are disjoint by
/// construction. This function intentionally remains budget arithmetic.
fn wave_capacity(tasks: &[Task], budget: i64) -> usize {
    let running_cost = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Dispatched | TaskStatus::Running | TaskStatus::Verifying
            )
        })
        .count() as i64;
    (budget - running_cost).max(0) as usize
}

pub fn compute_ready(tasks: &[Task], budget: i64) -> Vec<Task> {
    let done_keys: BTreeSet<&str> = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .map(|t| t.key.as_str())
        .collect();
    if wave_capacity(tasks, budget) == 0 {
        return Vec::new();
    }
    tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Pending)
        .filter(|t| {
            t.depends_on()
                .iter()
                .all(|dep| done_keys.contains(dep.as_str()))
        })
        .cloned()
        .collect()
}

/// Build the worker-operation payload as a **pure function of the
/// frozen task row** (§5.4 step 2): `stable_payload_hash` is then
/// deterministic, so a post-crash resubmit always idempotency-matches
/// the original operation instead of conflicting on payload hash.
pub fn build_worker_payload(task: &Task) -> Result<(&'static str, Value)> {
    match task.kind {
        TaskKind::Codex => {
            let payload = serde_json::to_value(CodexWorkerOperationPayload {
                actor: ActorId::KernelDispatcher,
                wave_id: task.wave_id.clone(),
                idempotency_key: task.id.clone(),
                goal: task.goal.clone(),
                // The workspace lease path created in
                // `CodexWorkerAdapter::prepare_tx` is the authoritative
                // worker cwd. `task.cwd` is intentionally not serialized:
                // prepare_tx would ignore it anyway, and including it would
                // change `stable_payload_hash` for in-flight Codex tasks
                // created by older builds when `plan.upsert` supplied a cwd,
                // causing a foreign-operation conflict after upgrade.
                cwd: None,
                context: serde_json::from_str(&task.context_json).unwrap_or(Value::Null),
                acceptance_criteria: task.acceptance_criteria.clone(),
            })?;
            Ok(("codex-worker", payload))
        }
        TaskKind::Claude => {
            let payload = serde_json::to_value(ClaudeWorkerOperationPayload {
                actor: ActorId::KernelDispatcher,
                wave_id: task.wave_id.clone(),
                idempotency_key: task.id.clone(),
                goal: task.goal.clone(),
                // The workspace lease path created in
                // `ClaudeWorkerAdapter::prepare_tx` is the authoritative
                // worker cwd. Keep task.cwd out of the payload hash just
                // like codex-worker.
                cwd: None,
                context: serde_json::from_str(&task.context_json).unwrap_or(Value::Null),
                acceptance_criteria: task.acceptance_criteria.clone(),
            })?;
            Ok(("claude-worker", payload))
        }
        TaskKind::Terminal => {
            let payload = serde_json::to_value(TerminalWorkerOperationPayload {
                actor: ActorId::KernelDispatcher,
                wave_id: task.wave_id.clone(),
                idempotency_key: task.id.clone(),
                cmd: task.goal.clone(),
                // Row value AS-IS — `None` stays `None` (#644 followup):
                // materializing `default_cwd()` (HOME/current dir) here
                // would make the payload — and therefore
                // `stable_payload_hash` — depend on process env, so a
                // restart under a different HOME would make
                // `resume_dispatched` see its OWN operation as a foreign
                // payload-hash conflict and permanently fail the task.
                // The terminal adapter resolves the default at spawn
                // time (`normalize_terminal_worker_cwd` in `prepare_tx`).
                cwd: task.cwd.clone(),
            })?;
            Ok(("terminal-worker", payload))
        }
    }
}

/// Child creation identity is a pure function of the post-claim row. Parent
/// cove/cwd are copied by the adapter inside its IMMEDIATE transaction so a
/// later wave patch cannot change this operation's payload hash.
pub fn build_child_wave_payload(task: &Task) -> Result<Value> {
    Ok(serde_json::to_value(ChildWaveOperationPayload {
        task_id: task.id.clone(),
        parent_wave_id: task.wave_id.clone(),
        goal: task.goal.clone(),
        acceptance: task.acceptance_criteria.clone(),
        context: serde_json::from_str(&task.context_json).unwrap_or(Value::Null),
        cwd: task.cwd.clone(),
    })?)
}

fn task_kind_str(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Codex => "codex",
        TaskKind::Claude => "claude",
        TaskKind::Terminal => "terminal",
    }
}

fn task_has_running_liveness_deadline(task: &Task) -> bool {
    task.spawn != "sub-wave" && matches!(task.kind, TaskKind::Codex | TaskKind::Claude)
}

fn duration_ms_i64(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// RAII guard for the per-task single-flight map: at most one in-process
/// driver (live scheduling pass OR sweep) submits/waits a given task's
/// worker operation at a time. Losing a slot is always safe — the holder
/// performs the same guarded writes — this just avoids duplicate
/// `wait()` polling.
struct InflightGuard {
    map: Arc<DashMap<String, ()>>,
    key: String,
}

impl InflightGuard {
    fn acquire(map: &Arc<DashMap<String, ()>>, key: &str) -> Option<Self> {
        use dashmap::mapref::entry::Entry;
        match map.entry(key.to_string()) {
            Entry::Occupied(_) => None,
            Entry::Vacant(v) => {
                v.insert(());
                Some(Self {
                    map: Arc::clone(map),
                    key: key.to_string(),
                })
            }
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.map.remove(&self.key);
    }
}

struct TimeoutCleanupSession {
    session_id: String,
    card_id: String,
}

#[derive(sqlx::FromRow)]
struct ChildTaskSnapshot {
    task_id: String,
    parent_wave_id: String,
    parent_cove_id: String,
    child_wave_id: String,
    child_lifecycle: Option<String>,
    gate_json: Option<String>,
    inflight_count: i64,
    pending_count: i64,
}

#[derive(Clone, Copy)]
enum ChildTerminalOutcome {
    Deleted,
    Failed,
    Canceled,
}

impl ChildTerminalOutcome {
    fn from_lifecycle(lifecycle: Option<WaveLifecycle>) -> Option<Self> {
        match lifecycle {
            None => Some(Self::Deleted),
            Some(WaveLifecycle::Failed) => Some(Self::Failed),
            Some(WaveLifecycle::Canceled) => Some(Self::Canceled),
            _ => None,
        }
    }

    const fn code(self) -> &'static str {
        match self {
            Self::Deleted => "child-wave-deleted",
            Self::Failed => "child-wave-failed",
            Self::Canceled => "child-wave-canceled",
        }
    }

    const fn detail(self) -> &'static str {
        match self {
            Self::Deleted => "child wave was deleted",
            Self::Failed => "child wave entered failed",
            Self::Canceled => "child wave was canceled",
        }
    }

    const fn sql_guard(self) -> &'static str {
        match self {
            Self::Deleted => "NOT EXISTS(SELECT 1 FROM waves child WHERE child.id=?5)",
            Self::Failed => {
                "EXISTS(SELECT 1 FROM waves child WHERE child.id=?5 AND child.lifecycle='failed')"
            }
            Self::Canceled => {
                "EXISTS(SELECT 1 FROM waves child WHERE child.id=?5 AND child.lifecycle='canceled')"
            }
        }
    }
}

/// Final child-success compare-and-set. The snapshot that selected the
/// success arm is advisory; this statement is the authority and rechecks the
/// child lifecycle plus quiescence in the writer transaction.
async fn guarded_child_success_flip_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
    child_wave_id: &str,
    target_status: &str,
    now: i64,
    finished_at_ms: Option<i64>,
) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE tasks SET status=?1,status_detail=NULL,worker_card_id=NULL,\
                running_deadline_ms=NULL,updated_at_ms=?2,finished_at_ms=?3 \
           WHERE id=?4 AND child_wave_id=?5 \
             AND status IN ('dispatched','running') \
             AND EXISTS(SELECT 1 FROM waves child \
                WHERE child.id=?5 AND child.lifecycle='done') \
             AND NOT EXISTS(SELECT 1 FROM tasks ct WHERE ct.wave_id=?5 \
                AND ct.status IN ('pending','dispatched','running','verifying'))",
    )
    .bind(target_status)
    .bind(now)
    .bind(finished_at_ms)
    .bind(task_id)
    .bind(child_wave_id)
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// Final pending-incomplete compare-and-set. Kept separate from the success
/// flip so each Done recheck has its own behavior oracle and mutation target.
async fn guarded_child_incomplete_flip_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
    parent_wave_id: &str,
    child_wave_id: &str,
    now: i64,
) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE tasks SET status='failed',status_detail='child-wave-incomplete',\
                worker_card_id=NULL,running_deadline_ms=NULL,updated_at_ms=?1,finished_at_ms=?1 \
           WHERE id=?2 AND wave_id=?3 AND child_wave_id=?4 \
             AND status IN ('dispatched','running') \
             AND EXISTS(SELECT 1 FROM waves child \
                WHERE child.id=?4 AND child.lifecycle='done') \
             AND NOT EXISTS(SELECT 1 FROM tasks ct WHERE ct.wave_id=?4 \
                AND ct.status IN ('dispatched','running','verifying')) \
             AND EXISTS(SELECT 1 FROM tasks ct WHERE ct.wave_id=?4 AND ct.status='pending')",
    )
    .bind(now)
    .bind(task_id)
    .bind(parent_wave_id)
    .bind(child_wave_id)
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// Final terminal-child compare-and-set shared by the deleted, failed and
/// canceled conclusions. The selected outcome comes from an advisory
/// snapshot; the outcome-specific SQL predicate is the authority.
async fn guarded_child_terminal_flip_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
    parent_wave_id: &str,
    child_wave_id: &str,
    outcome: ChildTerminalOutcome,
    now: i64,
) -> Result<u64> {
    let sql = format!(
        "UPDATE tasks SET status='failed',status_detail=?1,worker_card_id=NULL,\
                running_deadline_ms=NULL,updated_at_ms=?2,finished_at_ms=?2 \
           WHERE id=?3 AND wave_id=?4 AND child_wave_id=?5 \
             AND status IN ('dispatched','running') AND ({})",
        outcome.sql_guard()
    );
    Ok(sqlx::query(&sql)
        .bind(outcome.code())
        .bind(now)
        .bind(task_id)
        .bind(parent_wave_id)
        .bind(child_wave_id)
        .execute(&mut **tx)
        .await?
        .rows_affected())
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub async fn guarded_child_success_flip_for_test(
    tx: &mut Tx<'_>,
    task_id: &str,
    child_wave_id: &str,
) -> Result<u64> {
    let now = now_ms();
    guarded_child_success_flip_tx(tx, task_id, child_wave_id, "done", now, Some(now)).await
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub async fn guarded_child_incomplete_flip_for_test(
    tx: &mut Tx<'_>,
    task_id: &str,
    parent_wave_id: &str,
    child_wave_id: &str,
) -> Result<u64> {
    guarded_child_incomplete_flip_tx(tx, task_id, parent_wave_id, child_wave_id, now_ms()).await
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub async fn guarded_child_terminal_flip_for_test(
    tx: &mut Tx<'_>,
    task_id: &str,
    parent_wave_id: &str,
    child_wave_id: &str,
    lifecycle: Option<WaveLifecycle>,
) -> Result<u64> {
    let outcome = ChildTerminalOutcome::from_lifecycle(lifecycle).ok_or_else(|| {
        CalmError::Internal("terminal child flip fixture requires deleted/failed/canceled".into())
    })?;
    guarded_child_terminal_flip_tx(
        tx,
        task_id,
        parent_wave_id,
        child_wave_id,
        outcome,
        now_ms(),
    )
    .await
}

pub struct Scheduler {
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    /// Same `Weak` discipline as the dispatcher's `Inner` — the
    /// scheduler must not keep AppState resources alive after shutdown.
    operation_runtime: Weak<OperationRuntime>,
    /// The dispatcher's global spawn semaphore (§5.3): per-wave budgets
    /// cap per-wave parallelism, this caps total cross-wave spawn work.
    semaphore: Arc<Semaphore>,
    /// Kernel default budget (`NEIGE_WAVE_TASK_BUDGET`, default 1);
    /// `waves.task_budget` overrides per wave.
    budget_default: i64,
    /// Persisted running liveness window, resolved once from
    /// `NEIGE_TASK_RUN_TIMEOUT_SECS`.
    task_run_timeout: Duration,
    /// §5.1 per-wave single-flight: exactly the push-locks pattern.
    wave_locks: DashMap<WaveId, Arc<tokio::sync::Mutex<()>>>,
    /// Dirty flags — a trigger arriving mid-pass marks dirty and the
    /// lock holder loops once more, so no envelope is ever lost to "a
    /// pass was already running".
    wave_dirty: DashMap<WaveId, Arc<AtomicBool>>,
    /// Per-task single-flight for submit/wait drives (live + sweep).
    inflight: Arc<DashMap<String, ()>>,
    /// Round-3 review F2 — boot-order gate for the backstop sweeps.
    /// The dispatcher spawns the reconcile tick (and the Lagged-arm
    /// sweep) while `Dispatcher` is still being BUILT — before `main`
    /// runs `recover_operations_on_boot` → `scheduler_sweep_on_boot` —
    /// so an early tick/lag could run `sweep_all` against unrecovered
    /// operation rows. Both backstops funnel through
    /// [`Scheduler::sweep_all`], which no-ops until
    /// [`Scheduler::sweep_boot`] completes and opens this gate.
    boot_sweep_done: AtomicBool,
    /// #985 PR3a — dispatched recovery must not start until the boot
    /// context sweep has persisted every material verdict.
    context_sweep_boot_done: AtomicBool,
    context_metrics: Arc<ContextMetrics>,
    claim_fence_test_hook: std::sync::Mutex<Option<ClaimFenceTestHook>>,
    post_claim_drive_test_hook: std::sync::Mutex<Option<PostClaimDriveTestHook>>,
    #[cfg(feature = "fixtures")]
    reconcile_child_reopen_after_snapshot_test_hook: AtomicBool,
}

/// Deterministic integration-test rendezvous after closure resolution and
/// before the claim transaction begins.
#[doc(hidden)]
#[derive(Clone)]
pub struct ClaimFenceTestHook {
    pub resolved: Arc<Notify>,
    pub resume: Arc<Notify>,
}

/// Deterministic integration-test rendezvous after the claim commits and
/// before the frozen row is routed to an operation.
#[doc(hidden)]
#[derive(Clone)]
pub struct PostClaimDriveTestHook {
    pub claimed: Arc<Notify>,
    pub resume: Arc<Notify>,
}

impl Scheduler {
    pub fn new(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        operation_runtime: Weak<OperationRuntime>,
        semaphore: Arc<Semaphore>,
    ) -> Arc<Self> {
        Self::new_with_timeouts(
            repo,
            events,
            write,
            operation_runtime,
            semaphore,
            Self::task_run_timeout_from_env(),
        )
    }

    #[doc(hidden)]
    pub fn new_with_timeouts_for_test(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        operation_runtime: Weak<OperationRuntime>,
        semaphore: Arc<Semaphore>,
        task_run_timeout: Duration,
    ) -> Arc<Self> {
        Self::new_with_timeouts(
            repo,
            events,
            write,
            operation_runtime,
            semaphore,
            task_run_timeout,
        )
    }

    fn new_with_timeouts(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        operation_runtime: Weak<OperationRuntime>,
        semaphore: Arc<Semaphore>,
        task_run_timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            repo,
            events,
            write,
            operation_runtime,
            semaphore,
            budget_default: Self::budget_from_env(DEFAULT_WAVE_TASK_BUDGET),
            task_run_timeout,
            wave_locks: DashMap::new(),
            wave_dirty: DashMap::new(),
            inflight: Arc::new(DashMap::new()),
            boot_sweep_done: AtomicBool::new(false),
            context_sweep_boot_done: AtomicBool::new(false),
            context_metrics: Arc::new(ContextMetrics::default()),
            claim_fence_test_hook: std::sync::Mutex::new(None),
            post_claim_drive_test_hook: std::sync::Mutex::new(None),
            #[cfg(feature = "fixtures")]
            reconcile_child_reopen_after_snapshot_test_hook: AtomicBool::new(false),
        })
    }

    #[doc(hidden)]
    pub fn set_claim_fence_test_hook(&self, hook: ClaimFenceTestHook) {
        *self
            .claim_fence_test_hook
            .lock()
            .expect("claim fence hook lock") = Some(hook);
    }

    #[doc(hidden)]
    pub fn set_post_claim_drive_test_hook(&self, hook: PostClaimDriveTestHook) {
        *self
            .post_claim_drive_test_hook
            .lock()
            .expect("post-claim drive hook lock") = Some(hook);
    }

    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub fn reopen_child_after_reconcile_snapshot_for_test(&self) {
        self.reconcile_child_reopen_after_snapshot_test_hook
            .store(true, Ordering::SeqCst);
    }

    pub fn claim_fence_race_lost_count(&self) -> u64 {
        self.context_metrics.snapshot().claim_fence_race_lost
    }

    pub fn context_resolve_failure_count(&self, variant: &'static str) -> u64 {
        self.context_metrics
            .snapshot()
            .context_resolve_failures
            .get(variant)
            .copied()
            .unwrap_or(0)
    }

    pub fn context_metrics(&self) -> Arc<ContextMetrics> {
        Arc::clone(&self.context_metrics)
    }

    /// Resolve the kernel default budget from `NEIGE_WAVE_TASK_BUDGET`
    /// (parsed like `NEIGE_DISPATCHER_PERMITS`): unset / empty /
    /// unparseable / non-positive → `default`.
    pub fn budget_from_env(default: i64) -> i64 {
        match std::env::var("NEIGE_WAVE_TASK_BUDGET") {
            Ok(raw) => match raw.trim().parse::<i64>() {
                Ok(n) if n > 0 => n,
                _ => default,
            },
            Err(_) => default,
        }
    }

    /// Resolve a reconcile-tick period from an env var (non-positive /
    /// garbage → default).
    pub fn reconcile_secs_from_env_var(var: &str, default: u64) -> u64 {
        match std::env::var(var) {
            Ok(raw) => match raw.trim().parse::<u64>() {
                Ok(n) if n > 0 => n,
                _ => default,
            },
            Err(_) => default,
        }
    }

    /// Resolve the reconcile-tick period from
    /// `NEIGE_SCHEDULER_RECONCILE_SECS` (default 300; non-positive /
    /// garbage → default).
    pub fn reconcile_secs_from_env(default: u64) -> u64 {
        Self::reconcile_secs_from_env_var("NEIGE_SCHEDULER_RECONCILE_SECS", default)
    }

    pub fn task_run_timeout_from_env() -> Duration {
        Duration::from_secs(Self::reconcile_secs_from_env_var(
            "NEIGE_TASK_RUN_TIMEOUT_SECS",
            DEFAULT_TASK_RUN_TIMEOUT_SECS,
        ))
    }

    /// Configured kernel-default budget. Exposed for test assertions.
    pub fn budget_default(&self) -> i64 {
        self.budget_default
    }

    pub fn task_run_timeout_ms(&self) -> i64 {
        duration_ms_i64(self.task_run_timeout)
    }

    /// Fire-and-forget trigger: schedule the wave on a fresh task. Used
    /// by the dispatcher's envelope arms.
    pub fn poke(self: &Arc<Self>, wave_id: WaveId) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.schedule_wave(wave_id).await;
        });
    }

    /// Low-latency child lifecycle/deletion trigger. The event carries only a
    /// hint; the guarded IMMEDIATE transaction below rereads current DB state.
    pub fn reconcile_child_wave(self: &Arc<Self>, child_wave_id: WaveId) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = this.reconcile_child_wave_task(child_wave_id.as_str()).await {
                tracing::warn!(%error, %child_wave_id, "scheduler: live child-wave reconcile failed; sweep will retry");
            }
        });
    }

    async fn reconcile_all_child_wave_tasks(&self) {
        let Some(pool) = self.repo.sqlite_pool() else {
            return;
        };
        let child_ids: Vec<String> = match sqlx::query_scalar(
            "SELECT child_wave_id FROM tasks WHERE child_wave_id IS NOT NULL \
             AND status IN ('dispatched','running') ORDER BY created_at_ms",
        )
        .fetch_all(&pool)
        .await
        {
            Ok(ids) => ids,
            Err(error) => {
                tracing::warn!(%error, "scheduler sweep: child-wave scan failed");
                return;
            }
        };
        for child_id in child_ids {
            if let Err(error) = self.reconcile_child_wave_task(&child_id).await {
                tracing::warn!(%error, child_wave_id = %child_id, "scheduler sweep: child-wave reconcile failed");
            }
        }
    }

    async fn reconcile_child_wave_task(&self, child_wave_id: &str) -> Result<()> {
        let child_wave_id = child_wave_id.to_string();
        #[cfg(feature = "fixtures")]
        let reopen_child_after_snapshot = self
            .reconcile_child_reopen_after_snapshot_test_hook
            .swap(false, Ordering::SeqCst);
        let result = write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let snapshot: Option<ChildTaskSnapshot> = sqlx::query_as(
                        r#"SELECT t.id AS task_id, t.wave_id AS parent_wave_id,
                                  parent.cove_id AS parent_cove_id,
                                  t.child_wave_id AS child_wave_id,
                                  child.lifecycle AS child_lifecycle,
                                  t.gate_json AS gate_json,
                                  (SELECT count(*) FROM tasks ct
                                    WHERE ct.wave_id=t.child_wave_id
                                      AND ct.status IN ('dispatched','running','verifying')) AS inflight_count,
                                  (SELECT count(*) FROM tasks ct
                                    WHERE ct.wave_id=t.child_wave_id
                                      AND ct.status='pending') AS pending_count
                             FROM tasks t
                             JOIN waves parent ON parent.id=t.wave_id
                        LEFT JOIN waves child ON child.id=t.child_wave_id
                            WHERE t.child_wave_id=?1
                              AND t.status IN ('dispatched','running')"#,
                    )
                    .bind(&child_wave_id)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let Some(snapshot) = snapshot else {
                        return Err(race_lost_err());
                    };
                    #[cfg(feature = "fixtures")]
                    if reopen_child_after_snapshot {
                        sqlx::query("UPDATE waves SET lifecycle='planning' WHERE id=?1")
                            .bind(&snapshot.child_wave_id)
                            .execute(&mut **tx)
                            .await?;
                    }
                    let scope = EventScope::Wave {
                        wave: WaveId::from(snapshot.parent_wave_id.clone()),
                        cove: snapshot.parent_cove_id.clone().into(),
                    };
                    let now = now_ms();
                    let lifecycle = snapshot
                        .child_lifecycle
                        .as_deref()
                        .map(|value| WaveLifecycle::try_from(value.to_string()))
                        .transpose()
                        .map_err(|error| CalmError::Internal(format!("child lifecycle decode: {error}")))?;

                    if lifecycle == Some(WaveLifecycle::Done)
                        && snapshot.inflight_count == 0
                        && snapshot.pending_count == 0
                    {
                        let (target_status, finished_at) = if snapshot.gate_json.is_some() {
                            ("verifying", None)
                        } else {
                            ("done", Some(now))
                        };
                        let changed = guarded_child_success_flip_tx(
                            tx,
                            &snapshot.task_id,
                            &snapshot.child_wave_id,
                            target_status,
                            now,
                            finished_at,
                        )
                        .await?;
                        if changed == 0 {
                            return Err(race_lost_err());
                        }
                        let event = Event::TaskCompleted {
                            idempotency_key: snapshot.task_id,
                            result: json!({
                                "source": "child-wave",
                                "child_wave_id": snapshot.child_wave_id,
                            }),
                            artifacts: vec![],
                            agent_message: None,
                        };
                        return Ok(((), vec![(ActorId::KernelDispatcher, scope, event)]));
                    }

                    if lifecycle == Some(WaveLifecycle::Done)
                        && snapshot.inflight_count == 0
                        && snapshot.pending_count > 0
                    {
                        let changed = guarded_child_incomplete_flip_tx(
                            tx,
                            &snapshot.task_id,
                            &snapshot.parent_wave_id,
                            &snapshot.child_wave_id,
                            now,
                        )
                        .await?;
                        if changed == 0 {
                            return Err(race_lost_err());
                        }
                        let event = Event::TaskFailed {
                            idempotency_key: snapshot.task_id,
                            reason: format!(
                                "child-wave-incomplete: {} pending task(s) remain",
                                snapshot.pending_count
                            ),
                            agent_message: None,
                        };
                        return Ok(((), vec![(ActorId::KernelDispatcher, scope, event)]));
                    }

                    let outcome = ChildTerminalOutcome::from_lifecycle(lifecycle)
                        .ok_or_else(race_lost_err)?;
                    let changed = guarded_child_terminal_flip_tx(
                        tx,
                        &snapshot.task_id,
                        &snapshot.parent_wave_id,
                        &snapshot.child_wave_id,
                        outcome,
                        now,
                    )
                    .await?;
                    if changed == 0 {
                        return Err(race_lost_err());
                    }
                    let event = Event::TaskFailed {
                        idempotency_key: snapshot.task_id,
                        reason: format!("{}: {}", outcome.code(), outcome.detail()),
                        agent_message: None,
                    };
                    Ok(((), vec![(ActorId::KernelDispatcher, scope, event)]))
                })
            },
        )
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if is_race_lost(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub async fn reconcile_child_wave_for_test(&self, child_wave_id: &str) -> Result<()> {
        self.reconcile_child_wave_task(child_wave_id).await
    }

    /// Run scheduling passes for one wave until quiescent. Per-wave
    /// mutex + dirty flag: concurrent callers collapse into the lock
    /// holder's loop.
    pub async fn schedule_wave(self: &Arc<Self>, wave_id: WaveId) {
        let dirty = self
            .wave_dirty
            .entry(wave_id.clone())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone();
        dirty.store(true, Ordering::SeqCst);
        // IMPORTANT: do NOT bind the DashMap Entry to a `let` — the
        // shard guard must drop at this statement's `;` before the
        // `.await` below (same hazard as the dispatcher's push locks).
        let lock = self
            .wave_locks
            .entry(wave_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        while dirty.swap(false, Ordering::SeqCst) {
            if let Err(e) = self.schedule_pass(&wave_id).await {
                tracing::warn!(
                    wave_id = %wave_id,
                    error = %e,
                    "scheduler: scheduling pass failed; will retry on next trigger/sweep"
                );
            }
        }
    }

    /// One §5.2 pass under the wave lock: lifecycle gate → budget →
    /// ready set → dispatch each ready task sequentially.
    async fn schedule_pass(self: &Arc<Self>, wave_id: &WaveId) -> Result<()> {
        let Some(wave) = self.repo.wave_get(wave_id.as_str()).await? else {
            return Ok(());
        };
        let tasks = self.repo.tasks_by_wave(wave_id.as_str()).await?;
        // §6.2 trigger 2 — the emit-tx flip already moved gated rows to
        // `verifying`; this pass (poked by the `task.completed`
        // envelope) drives each one's gate. Fire-and-forget: a gate can
        // run for minutes-to-hours and must never block the wave lock;
        // `drive_gate`'s single-flight guard collapses duplicates.
        // Deliberately BEFORE the lifecycle gate (PR #685 F6): §5.2
        // scopes lifecycle gating to NEW claims; a gate for a task that
        // reported while the wave is Blocked is in-flight machinery and
        // must not wait for the reconcile tick.
        for task in tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Verifying)
            .cloned()
        {
            let this = Arc::clone(self);
            tokio::spawn(async move {
                this.drive_gate(task).await;
            });
        }
        if !lifecycle_allows_scheduling(wave.lifecycle) {
            tracing::debug!(
                wave_id = %wave_id,
                lifecycle = ?wave.lifecycle,
                "scheduler: lifecycle holds scheduling; skipping pass"
            );
            return Ok(());
        }
        let budget = self.wave_budget(wave_id).await?;
        let capacity = wave_capacity(&tasks, budget);
        let ready = compute_ready(&tasks, budget);
        let mut claimed = 0;
        for task in ready {
            if self.dispatch_task(task, &wave).await {
                claimed += 1;
                if claimed == capacity {
                    break;
                }
            }
        }
        Ok(())
    }

    /// `COALESCE(waves.task_budget, kernel default)` (§5.3).
    async fn wave_budget(&self, wave_id: &WaveId) -> Result<i64> {
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("scheduler requires a sqlite-backed Repo".into()))?;
        let row: Option<(Option<i64>,)> =
            sqlx::query_as("SELECT task_budget FROM waves WHERE id = ?1")
                .bind(wave_id.as_str())
                .fetch_optional(&pool)
                .await?;
        Ok(row
            .and_then(|(budget,)| budget)
            .unwrap_or(self.budget_default)
            .max(0))
    }

    /// §5.4 — claim one ready task and drive its worker spawn. Every
    /// failure mode is contained here (logged, row reconciled); the
    /// pass continues with its remaining ready tasks.
    async fn dispatch_task(self: &Arc<Self>, task: Task, wave: &Wave) -> bool {
        let Some(_inflight) = InflightGuard::acquire(&self.inflight, &task.id) else {
            tracing::debug!(task_id = %task.id, "scheduler: task already in flight; skipping");
            return false;
        };
        // Global spawn cap — same semaphore the dispatcher holds across
        // its spawn handling.
        let _permit = match Arc::clone(&self.semaphore).acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("scheduler: dispatcher semaphore closed; skipping dispatch");
                return false;
            }
        };
        // The spawn is driven off the row the claim tx itself re-read
        // AFTER winning (review F2): the semaphore wait above leaves an
        // unbounded window in which a still-pending row can be revised
        // or re-kinded, so the pre-claim snapshot must never feed the
        // payload. Post-claim the row is frozen — every plan mutation
        // path is `WHERE status = 'pending'`.
        let pre_claim_task_id = task.id.clone();
        let frozen = match self.claim_task(task, wave).await {
            Ok(Some(frozen)) => frozen,
            Ok(None) => return false, // someone else won the claim
            Err(e) => {
                tracing::warn!(
                    task_id = %pre_claim_task_id,
                    error = %e,
                    "scheduler: claim tx failed; task stays pending for the next trigger"
                );
                return false;
            }
        };
        let post_claim_hook = self
            .post_claim_drive_test_hook
            .lock()
            .expect("post-claim drive hook lock")
            .take();
        if let Some(hook) = post_claim_hook {
            hook.claimed.notify_one();
            hook.resume.notified().await;
        }
        if let Err(e) = self.drive_spawn(&frozen, wave).await {
            tracing::warn!(
                task_id = %pre_claim_task_id,
                error = %e,
                "scheduler: worker spawn drive failed; sweep will reconcile"
            );
        }
        true
    }

    /// The claim tx (§5.4 step 1, one eventized write): in-tx lifecycle
    /// re-check, single-winner `pending → dispatched` UPDATE,
    /// `Event::TaskDispatched` (§5.6), and the lifecycle auto-promotion
    /// to `Working`, all in one tx.
    ///
    /// Returns the post-claim re-read of the row — the **frozen** task
    /// (review F2): pending rows are mutable right up to the claim, so
    /// the dispatch payload must be built from what was actually
    /// claimed, never from the caller's pre-claim snapshot. Post-claim
    /// the row cannot change shape (all plan mutation paths are
    /// `WHERE status = 'pending'`), so a post-crash sweep resubmit
    /// rebuilds the byte-identical payload.
    ///
    /// `Ok(None)` = race lost: another claimer won, the wave's
    /// lifecycle left the schedulable set since the ready-set pass
    /// (review F4), the frozen row's ready predicate no longer holds
    /// (round-2 review F1), or the wave row was deleted. No event is
    /// persisted.
    async fn claim_task(&self, task: Task, wave: &Wave) -> Result<Option<Task>> {
        let monitor = TaskContextMonitor::new_with_metrics(
            Arc::clone(&self.repo),
            self.events.clone(),
            self.write.clone(),
            Arc::clone(&self.context_metrics),
        );
        let closure = match monitor.resolve_task_closure(&task.wave_id, &task.key).await {
            Ok(closure) => closure,
            Err(error) => {
                let variant = error.variant();
                self.context_metrics.record_context_resolve_failure(variant);
                tracing::warn!(
                    task_id = %task.id,
                    resolve_error_variant = variant,
                    error = ?error,
                    "scheduler: claim context resolution failed; task stays pending"
                );
                return Ok(None);
            }
        };
        let scope = EventScope::Wave {
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let task_id = task.id.clone();
        let wave_id = wave.id.clone();
        let budget_default = self.budget_default;
        let claim_refs = closure.refs;
        let claim_doc_revs = closure.doc_revs;
        let claim_truncated = closure.closure_truncated;
        let task_key = task.key.clone();
        let test_hook = self
            .claim_fence_test_hook
            .lock()
            .expect("claim fence hook lock")
            .take();
        if let Some(hook) = test_hook {
            hook.resolved.notify_one();
            hook.resume.notified().await;
        }
        let context_metrics = Arc::clone(&self.context_metrics);
        let result =
            write_with_actor_events_typed::<Task, _>(
                self.repo.as_ref(),
                None,
                &self.events,
                &self.write,
                move |tx| {
                    Box::pin(async move {
                        // §5.2 lifecycle gate, re-checked IN the claim tx
                        // (review F4): the pass's pre-claim read can go
                        // stale across the semaphore wait, and a wave moved
                        // to Blocked/Canceled/Done must not have new work
                        // claimed. Loss is silent (race-lost, no event).
                        let (lifecycle, task_budget) =
                            wave_lifecycle_and_budget_tx(tx, wave_id.as_str())
                                .await?
                                .ok_or_else(race_lost_err)?;
                        if !lifecycle_allows_scheduling(lifecycle) {
                            return Err(race_lost_err());
                        }
                        // §5.2 claim fence: missing wave/report, a changed root, or any
                        // changed report doc_rev is a silent race loss. This runs before
                        // the pending -> dispatched state flip.
                        for (frozen_wave, frozen_rev) in &claim_doc_revs {
                            let current: Option<Option<i64>> = match sqlx::query_as::<_, (Option<i64>,)>(
                                "SELECT json_extract(c.payload, '$.docRev') FROM cards c \
                                 JOIN waves w ON w.id = c.wave_id \
                                 WHERE c.wave_id = ?1 AND c.kind = 'wave-report' LIMIT 1",
                            )
                            .bind(frozen_wave)
                            .fetch_optional(&mut **tx)
                            .await
                            {
                                Ok(row) => row.map(|(value,)| value),
                                Err(error) => {
                                    tracing::warn!(
                                        wave_id = frozen_wave,
                                        error = %error,
                                        "scheduler: claim fence doc_rev query failed; failing closed"
                                    );
                                    context_metrics.record_claim_fence_race_lost();
                                    return Err(race_lost_err());
                                }
                            };
                            if !fence_revision_matches(current, *frozen_rev) {
                                context_metrics.record_claim_fence_race_lost();
                                return Err(race_lost_err());
                            }
                        }
                        if let Some(root) = claim_refs.iter().find(|reference| reference.is_root) {
                            let payload: Option<String> = sqlx::query_scalar(
                                "SELECT payload FROM cards WHERE wave_id = ?1 \
                                 AND kind = 'wave-report' LIMIT 1",
                            )
                            .bind(root.wave_id.as_str())
                            .fetch_optional(&mut **tx)
                            .await?;
                            let current_root = payload
                                .and_then(|payload| serde_json::from_str::<Value>(&payload).ok())
                                .and_then(|payload| {
                                    payload.get("blocks").and_then(Value::as_array).cloned()
                                })
                                .and_then(|blocks| {
                                    blocks.into_iter().find_map(|value| {
                                        let block: calm_types::wave_report::ReportBlock =
                                            serde_json::from_value(value).ok()?;
                                        if block.id != root.block_id {
                                            return None;
                                        }
                                        Some(context_ref(root.wave_id.as_str(), &block, true))
                                    })
                                });
                            if current_root
                                .as_ref()
                                .map(|current| (&current.block_id, &current.hash))
                                != Some((&root.block_id, &root.hash))
                            {
                                context_metrics.record_claim_fence_race_lost();
                                return Err(race_lost_err());
                            }
                        }
                        let now = now_ms();
                        let rows =
                            task_claim_pending_tx(tx, &task_id, now, &claim_refs, claim_truncated)
                                .await?;
                        if rows == 0 {
                            return Err(race_lost_err());
                        }
                        // Post-claim re-read = the frozen row (review F2).
                        // Gone row = concurrent wave delete; treat as lost.
                        let frozen = task_get_tx(tx, &task_id).await?.ok_or_else(race_lost_err)?;
                        // Round-2 review F1: revalidate the §5.2 ready
                        // predicate against the wave's CURRENT plan in the
                        // same tx. The pass's ready set was computed before
                        // the semaphore wait, so a `plan.updated` that added
                        // a dependency or a PATCH that shrank the budget
                        // mid-window must abort the claim (race-lost, the
                        // rollback un-flips the row, the next poke
                        // re-evaluates). Strict priority ORDER is
                        // deliberately NOT revalidated — the design only
                        // fixes the ready-set order per pass (§5.4).
                        let siblings = tasks_by_wave_tx(tx, wave_id.as_str()).await?;
                        let done_keys: BTreeSet<&str> = siblings
                            .iter()
                            .filter(|t| t.status == TaskStatus::Done)
                            .map(|t| t.key.as_str())
                            .collect();
                        if !frozen
                            .depends_on()
                            .iter()
                            .all(|dep| done_keys.contains(dep.as_str()))
                        {
                            return Err(race_lost_err());
                        }
                        let budget = task_budget.unwrap_or(budget_default).max(0);
                        // `siblings` was read AFTER the claim flip, so the
                        // in-flight count includes this row — it must fit
                        // the budget, not stay strictly under it.
                        let in_flight = siblings
                            .iter()
                            .filter(|t| {
                                matches!(
                                    t.status,
                                    TaskStatus::Dispatched
                                        | TaskStatus::Running
                                        | TaskStatus::Verifying
                                )
                            })
                            .count() as i64;
                        if in_flight > budget {
                            return Err(race_lost_err());
                        }
                        let mut events = vec![
                            (
                                ActorId::KernelDispatcher,
                                scope.clone(),
                                Event::TaskDispatched {
                                    idempotency_key: task_id.clone(),
                                    kind: task_kind_str(frozen.kind).to_string(),
                                    agent_message: Some(format!(
                                        "[scheduler] dispatching task {}",
                                        frozen.key
                                    )),
                                },
                            ),
                            (
                                ActorId::KernelDispatcher,
                                scope.clone(),
                                Event::TaskContextFrozen {
                                    wave_id: wave_id.clone(),
                                    task_key: task_key.clone(),
                                    idempotency_key: task_id.clone(),
                                    task_id: task_id.clone(),
                                    refs: claim_refs.clone(),
                                    doc_revs: claim_doc_revs.clone(),
                                    truncated: claim_truncated,
                                },
                            ),
                        ];
                        // Same pre-spawn ordering rationale as the legacy
                        // dispatch path: promote before the worker exists so
                        // a fast report's Working → Reviewing promotion can
                        // never race ahead of this one. §5.2 deliberately
                        // schedules Planning waves (review F5) — a wave the
                        // spec never moved past Planning is promoted along
                        // the legal kernel chain Planning → Dispatching →
                        // Working here. §5.2 also keeps Reviewing in the
                        // schedulable set (round-5 review F1): a dependent
                        // task that becomes ready after the first worker's
                        // completion promoted the wave to Reviewing is
                        // claimed from Reviewing, so the legal Reviewing →
                        // Working edge rides the same claim tx. A
                        // successful claim therefore always leaves the wave
                        // `Working` and the later Working → Reviewing
                        // auto-transition can fire again.
                        for (from, to) in [
                            (WaveLifecycle::Reviewing, WaveLifecycle::Working),
                            (WaveLifecycle::Planning, WaveLifecycle::Dispatching),
                            (WaveLifecycle::Dispatching, WaveLifecycle::Working),
                        ] {
                            if let Some(auto_events) = auto_transition_if_current_in_tx(
                                tx,
                                &wave_id,
                                from,
                                to,
                                &ActorId::KernelDispatcher,
                                Some("[auto] scheduler claimed a task".to_string()),
                            )
                            .await?
                            {
                                events.extend(auto_events.into_iter().map(|event| {
                                    (ActorId::KernelDispatcher, scope.clone(), event)
                                }));
                            }
                        }
                        Ok((frozen, events))
                    })
                },
            )
            .await;
        match result {
            Ok((frozen, _)) => Ok(Some(frozen)),
            Err(e) if is_race_lost(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// §5.4 steps 2-3 — build the deterministic payload, submit the
    /// worker operation (`idempotency_key = task.id`; duplicate submits
    /// dedupe on the operations unique index), `wait()` it to a
    /// terminal phase, then reconcile the task row with guarded writes.
    ///
    /// Shared verbatim between the live dispatch path and the sweep's
    /// `dispatched` arm: `submit` on an existing key returns the
    /// existing op id (missing → resubmit), and `wait()` is the
    /// steady-state re-drive — the one public API that re-polls
    /// `drive()` until the op is terminal (no background driver exists,
    /// §8). The drive lease (60s, `claim_drive_batch`) makes concurrent
    /// drivers execute no phase twice.
    async fn drive_spawn(&self, task: &Task, wave: &Wave) -> Result<()> {
        if task.spawn == "sub-wave" {
            return self.drive_child_wave(task, wave).await;
        }
        let Some(runtime) = self.operation_runtime.upgrade() else {
            tracing::debug!(
                task_id = %task.id,
                "scheduler: operation runtime dropped; skipping spawn drive"
            );
            return Ok(());
        };
        let (op_kind, payload) = build_worker_payload(task)?;
        let payload_hash = stable_payload_hash(&payload)?;
        let op_id = match runtime
            .submit(
                op_kind,
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(task.id.clone()),
                    payload_hash,
                },
                payload,
            )
            .await
        {
            Ok(op_id) => op_id,
            // Round-3 review F1 — error classification. The idempotency
            // payload-hash conflict is PERMANENT: `build_worker_payload`
            // is a pure function of the frozen post-claim row, so OUR
            // resubmits always hash-match the original operation; a
            // mismatch under this task's key can only be a foreign
            // operation (e.g. a legacy `calm.task.dispatch` spawn that
            // already used the task id) — it will never self-heal, and
            // leaving the row `dispatched` would retry the same error
            // every sweep while pinning the wave budget forever. Run the
            // same spawn-failure path as an op Failed/Stuck outcome:
            // guarded `failed('spawn-failed')` + kernel `task.failed`.
            Err(e) if crate::operation::is_idempotency_payload_conflict(&e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "scheduler: task idempotency key owned by a foreign operation (permanent); failing task"
                );
                return self.fail_spawn(task, wave, &e.to_string()).await;
            }
            // Everything else stays TRANSIENT/unknown (policy-free: no
            // retry counting) — log-and-leave for the next trigger/sweep.
            Err(e) => return Err(e),
        };
        let result = runtime.wait(&op_id).await?;
        self.reconcile_spawn_result(task, wave, result.outcome)
            .await
    }

    async fn drive_child_wave(&self, task: &Task, wave: &Wave) -> Result<()> {
        let Some(runtime) = self.operation_runtime.upgrade() else {
            tracing::debug!(task_id = %task.id, "scheduler: operation runtime dropped; skipping child-wave drive");
            return Ok(());
        };
        let payload = build_child_wave_payload(task)?;
        let op_id = runtime
            .submit(
                CHILD_WAVE_KIND,
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(task.id.clone()),
                    payload_hash: stable_payload_hash(&payload)?,
                },
                payload,
            )
            .await?;
        let child_result = runtime.wait(&op_id).await?;
        let result = match child_result.outcome {
            OperationOutcome::Succeeded { result }
            | OperationOutcome::SucceededViaCollision { result, .. } => result,
            OperationOutcome::Failed { last_error, .. } => {
                let code = if last_error.contains("sub-wave-depth-exceeded") {
                    "sub-wave-depth-exceeded"
                } else if last_error.contains("sub-wave-tree-cycle") {
                    "sub-wave-tree-cycle"
                } else {
                    "child-wave-create-failed"
                };
                return self
                    .fail_child_wave_task(task, wave, code, &last_error)
                    .await;
            }
            OperationOutcome::Stuck { reason, .. } => {
                return self
                    .fail_child_wave_task(task, wave, "child-wave-create-stuck", &reason)
                    .await;
            }
        };
        let child_id = result
            .get("child_wave_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("child-wave result missing child_wave_id".into()))?;
        let spec_card_id = result
            .get("spec_card_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("child-wave result missing spec_card_id".into()))?;
        let report_card_id = result
            .get("report_card_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CalmError::Internal("child-wave result missing report_card_id".into())
            })?;
        let cwd = result
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("child-wave result missing cwd".into()))?;
        let seed = result
            .get("seed")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("child-wave result missing seed".into()))?;

        let bootstrap = SpecHarnessStartOperationPayload {
            actor: ActorId::KernelDispatcher,
            wave_id: child_id.to_string(),
            spec_card_id: crate::ids::CardId::from(spec_card_id.to_string()),
            report_card_id: Some(report_card_id.to_string()),
            sort: None,
            cwd: cwd.to_string(),
            goal: Some(seed.to_string()),
            reset_harness_items: false,
            force_new_thread: false,
            profile: Default::default(),
            create_card: None,
            first_message_sha256: None,
        };
        let bootstrap_payload = serde_json::to_value(&bootstrap)?;
        let bootstrap_id = runtime
            .submit(
                "spec-harness-start",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(format!("child-wave:{child_id}:bootstrap")),
                    payload_hash: stable_payload_hash(&bootstrap_payload)?,
                },
                bootstrap_payload,
            )
            .await?;
        // Bootstrap is strictly before the dispatched→running flip. A crash
        // can therefore only leave a dispatched row, which resume re-drives.
        match runtime.wait(&bootstrap_id).await?.outcome {
            OperationOutcome::Succeeded { .. } | OperationOutcome::SucceededViaCollision { .. } => {
                self.mark_sub_wave_running(&task.id).await
            }
            OperationOutcome::Failed { last_error, .. } => {
                self.fail_child_wave_task(task, wave, "child-wave-bootstrap-failed", &last_error)
                    .await
            }
            OperationOutcome::Stuck { reason, .. } => {
                self.fail_child_wave_task(task, wave, "child-wave-bootstrap-stuck", &reason)
                    .await
            }
        }
    }

    async fn mark_sub_wave_running(&self, task_id: &str) -> Result<()> {
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("scheduler requires a sqlite-backed Repo".into()))?;
        let mut tx = begin_immediate_tx(&pool).await?;
        task_mark_sub_wave_running_tx(&mut tx, task_id, now_ms()).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn fail_child_wave_task(
        &self,
        task: &Task,
        wave: &Wave,
        code: &str,
        detail: &str,
    ) -> Result<()> {
        let task_id = task.id.clone();
        let wave_id = wave.id.clone();
        let scope = EventScope::Wave {
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let code = code.to_string();
        let reason = format!("{code}: {detail}");
        let result = write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    // The child-wave operation may fail after prepare_tx has
                    // committed the child and stamped this row. Always derive
                    // cleanup ownership from durable task state in the same
                    // transaction as the parent failure flip; callers cannot
                    // reliably infer child existence from an operation phase.
                    let child_id: Option<String> = sqlx::query_scalar(
                        "SELECT child_wave_id FROM tasks WHERE id=?1 AND wave_id=?2",
                    )
                    .bind(&task_id)
                    .bind(wave_id.as_str())
                    .fetch_optional(&mut **tx)
                    .await?
                    .flatten();
                    let rows = task_fail_from_worker_tx(
                        tx,
                        &task_id,
                        wave_id.as_str(),
                        TaskReporter::Kernel,
                        &code,
                        now_ms(),
                    )
                    .await?;
                    if rows == 0 {
                        return Err(race_lost_err());
                    }
                    let mut events = Vec::new();
                    if let Some(child_id) = child_id {
                        let child_wave_id = WaveId::from(child_id);
                        let current =
                            crate::wave_lifecycle::wave_get_tx(tx, &child_wave_id).await?;
                        if !current.lifecycle.is_terminal() {
                            let updated = crate::db::sqlite::wave_update_tx(
                                tx,
                                child_wave_id.as_str(),
                                crate::model::WavePatch {
                                    lifecycle: Some(WaveLifecycle::Failed),
                                    ..Default::default()
                                },
                            )
                            .await?;
                            let child_scope = EventScope::Wave {
                                wave: updated.id.clone(),
                                cove: updated.cove_id.clone(),
                            };
                            events.push((
                                child_scope.clone(),
                                Event::WaveLifecycleChanged {
                                    id: updated.id.clone(),
                                    cove_id: updated.cove_id.clone(),
                                    from: current.lifecycle,
                                    to: WaveLifecycle::Failed,
                                    agent_message: Some(reason.clone()),
                                },
                            ));
                            events.push((
                                child_scope,
                                Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(
                                    updated,
                                    Some(reason.clone()),
                                )),
                            ));
                        }
                    }
                    events.push((
                        scope,
                        Event::TaskFailed {
                            idempotency_key: task_id,
                            reason,
                            agent_message: None,
                        },
                    ));
                    Ok((
                        (),
                        events
                            .into_iter()
                            .map(|(scope, event)| (ActorId::KernelDispatcher, scope, event))
                            .collect(),
                    ))
                })
            },
        )
        .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if is_race_lost(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn reconcile_spawn_result(
        &self,
        task: &Task,
        wave: &Wave,
        outcome: OperationOutcome,
    ) -> Result<()> {
        match outcome {
            OperationOutcome::Succeeded { result }
            | OperationOutcome::SucceededViaCollision { result, .. } => {
                // §3: guarded `dispatched → running` + two-sided
                // `worker_card_id` stamp. The op result for the worker
                // kinds is the created card row; a missing id leaves the
                // stamp to the report tx's COALESCE.
                let worker_card_id = result.get("id").and_then(Value::as_str).map(str::to_string);
                self.mark_running(&task.id, worker_card_id.as_deref())
                    .await?;
                // Review F6: a terminal task resumed by the boot sweep
                // may already carry a recorded exit (the PTY died while
                // the kernel was down and the supervisor reconcile
                // persisted it). Reconcile right now instead of leaving
                // the row `running` until the next periodic sweep. The
                // live spawn path shares this check harmlessly — a
                // just-spawned terminal has no exit record, so it
                // no-ops.
                if task.kind == TaskKind::Terminal {
                    match self.repo.task_get(&task.id).await {
                        Ok(Some(row)) if row.status == TaskStatus::Running => {
                            self.reconcile_running_terminal(row).await;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(
                                task_id = %task.id,
                                error = %e,
                                "scheduler: post-stamp terminal re-read failed; sweep will reconcile"
                            );
                        }
                    }
                }
            }
            OperationOutcome::Failed { last_error, .. } => {
                self.fail_spawn(task, wave, &last_error).await?;
            }
            OperationOutcome::Stuck { reason, .. } => {
                self.fail_spawn(task, wave, &reason).await?;
            }
        }
        Ok(())
    }

    /// Guarded running stamp. No event rides along (the dispatch record
    /// already landed in the claim tx), so this is a plain guarded
    /// UPDATE: 0 rows = a fast worker report already advanced the row —
    /// by design, not an error.
    async fn mark_running(&self, task_id: &str, worker_card_id: Option<&str>) -> Result<()> {
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("scheduler requires a sqlite-backed Repo".into()))?;
        let mut tx = begin_immediate_tx(&pool).await?;
        let now = now_ms();
        let rows = task_mark_running_tx(
            &mut tx,
            task_id,
            worker_card_id,
            now,
            now.saturating_add(self.task_run_timeout_ms()),
        )
        .await?;
        tx.commit().await?;
        if rows == 0 {
            tracing::debug!(
                task_id = %task_id,
                "scheduler: running stamp no-op (fast worker report already advanced the row)"
            );
        }
        Ok(())
    }

    /// Spawn failure/stuck (§5.4 step 3): guarded
    /// `dispatched/running → failed('spawn-failed')` + kernel
    /// `task.failed` (actor `KernelDispatcher`) in one tx so the spec
    /// gets pushed, + the same `Working → Reviewing` promotion the
    /// legacy spawn-failure path performs. 0-row flip → the row already
    /// moved on; no event is emitted.
    async fn fail_spawn(&self, task: &Task, wave: &Wave, reason: &str) -> Result<()> {
        let scope = EventScope::Wave {
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let task_id = task.id.clone();
        let wave_id = wave.id.clone();
        let reason = format!("worker spawn failed: {reason}");
        let result = write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let rows = task_fail_from_worker_tx(
                        tx,
                        &task_id,
                        wave_id.as_str(),
                        TaskReporter::Kernel,
                        "spawn-failed",
                        now_ms(),
                    )
                    .await?;
                    if rows == 0 {
                        return Err(race_lost_err());
                    }
                    let mut events = vec![(
                        ActorId::KernelDispatcher,
                        scope.clone(),
                        Event::TaskFailed {
                            idempotency_key: task_id.clone(),
                            reason,
                            agent_message: None,
                        },
                    )];
                    if let Some(auto_events) = auto_transition_if_current_in_tx(
                        tx,
                        &wave_id,
                        WaveLifecycle::Working,
                        WaveLifecycle::Reviewing,
                        &ActorId::KernelDispatcher,
                        Some("[auto] worker spawn failed".to_string()),
                    )
                    .await?
                    {
                        events.extend(
                            auto_events
                                .into_iter()
                                .map(|event| (ActorId::KernelDispatcher, scope.clone(), event)),
                        );
                    }
                    Ok(((), events))
                })
            },
        )
        .await;
        match result {
            Ok(_) => Ok(()),
            // 0-row flip: the row already moved on (e.g. a late worker
            // report landed first) — nothing to record.
            Err(e) if is_race_lost(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// §8 sweep body — shared between boot (after operation recovery),
    /// the periodic reconcile tick, and `Lagged`. Arms for this slice:
    ///
    /// - `pending`: recompute ready sets and dispatch (per wave).
    /// - `dispatched`: resubmit/re-drive the worker op and reconcile
    ///   the row ([`Scheduler::drive_spawn`] — `submit` dedupes on the
    ///   idempotency key, `wait()` re-drives non-terminal ops, the
    ///   guarded writes reconcile terminal outcomes).
    /// - `running` + terminal kind: mechanically reconcilable — the
    ///   boot supervisor reconcile has already persisted dead PTYs as
    ///   `terminals.exit_code = -1`, so a recorded exit runs the same
    ///   guarded completion tx as the live exit hook.
    /// - `running` + codex kind: fail only after the persisted
    ///   wall-clock liveness deadline, CAS first, then teardown.
    /// - `verifying`: drive the current gate attempt
    ///   ([`Scheduler::drive_gate`] — submit when missing, `wait()`
    ///   re-drive when non-terminal, outcome copy when terminal); the
    ///   parked-op enforcement arms (dead probe / deadline) run via
    ///   `OperationRuntime::sweep_parked` at the top of the body.
    ///
    /// Boot-gated (round-3 review F2): both backstop callers — the
    /// reconcile tick and the Lagged arm — are spawned during
    /// `Dispatcher` construction, BEFORE `main`'s
    /// `recover_operations_on_boot` → `scheduler_sweep_on_boot` funnel,
    /// so a sweep here before [`Scheduler::sweep_boot`] completes could
    /// re-drive dispatched rows against unrecovered operation rows.
    /// Until the gate opens this is a no-op; nothing is lost — the boot
    /// sweep itself covers everything an early tick/lag would have.
    pub async fn sweep_all(self: &Arc<Self>) {
        if !self.boot_sweep_done.load(Ordering::SeqCst) {
            tracing::debug!(
                "scheduler: backstop sweep skipped — boot recovery/sweep has not completed yet"
            );
            return;
        }
        let pending_waves = self.sweep_reconcile().await;
        for wave_id in pending_waves {
            self.schedule_wave(WaveId::from(wave_id)).await;
        }
    }

    /// Boot-time sweep (§8 + review F7): the reconcile arms
    /// (`dispatched` re-drive, `running`-terminal recorded-exit) run
    /// synchronously — they must complete in boot order, after
    /// operation recovery — but pending-arm dispatching goes through
    /// the normal async [`Scheduler::poke`] path so boot never blocks
    /// the HTTP server behind full schedule passes (claim + spawn
    /// `wait()` per wave).
    ///
    /// Completing this sweep opens the boot gate (round-3 review F2):
    /// from here on the periodic reconcile tick and Lagged-arm
    /// [`Scheduler::sweep_all`] calls run for real.
    pub async fn sweep_boot(self: &Arc<Self>) {
        let pending_waves = self.sweep_reconcile().await;
        for wave_id in pending_waves {
            self.poke(WaveId::from(wave_id));
        }
        self.boot_sweep_done.store(true, Ordering::SeqCst);
    }

    /// Round-3 review F2 — whether the boot gate is open (the boot
    /// sweep completed). Exposed for test assertions.
    pub fn boot_sweep_completed(&self) -> bool {
        self.boot_sweep_done.load(Ordering::SeqCst)
    }

    /// TEST seam: open the boot gate without running a boot sweep, for
    /// suites that drive [`Scheduler::sweep_all`] / scheduling passes
    /// directly. Production only opens the gate via
    /// [`Scheduler::sweep_boot`] (the `scheduler_sweep_on_boot` funnel).
    pub fn mark_boot_sweep_complete(&self) {
        self.boot_sweep_done.store(true, Ordering::SeqCst);
    }

    /// Test-only steady-state seam. Production opens this gate only after a
    /// successful context sweep via [`Scheduler::open_context_sweep_gate`].
    #[cfg(any(test, feature = "fixtures"))]
    pub fn mark_context_sweep_boot_complete(&self) {
        self.context_sweep_boot_done.store(true, Ordering::SeqCst);
    }

    /// A successful full context sweep opens the resume gate exactly once.
    /// The winner immediately retries dispatched rows in the same turn.
    pub async fn open_context_sweep_gate(self: &Arc<Self>) -> bool {
        let opened = self
            .context_sweep_boot_done
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if opened {
            self.sweep_all().await;
        }
        opened
    }

    /// Shared sweep body: runs the reconcile arms inline and returns
    /// the set of waves holding `pending` rows for the caller to
    /// dispatch (blocking in [`Scheduler::sweep_all`], fire-and-forget
    /// in [`Scheduler::sweep_boot`]).
    async fn sweep_reconcile(self: &Arc<Self>) -> BTreeSet<String> {
        // #653 §4.4 call-site (c): the consumer reconcile tick runs the
        // saga's parked sweep — recovers durable verdicts from dead
        // gates (pre-deadline dead-probe) and kill-fails past-deadline
        // work. Every arm is fenced single-winner, so racing the live
        // observer / boot recovery is safe.
        if let Some(runtime) = self.operation_runtime.upgrade()
            && let Err(e) = runtime.sweep_parked().await
        {
            tracing::warn!(error = %e, "scheduler sweep: sweep_parked failed; next tick retries");
        }
        self.sweep_timeout_worker_cleanups().await;
        let mut pending_waves: BTreeSet<String> = BTreeSet::new();
        let tasks = match self.repo.tasks_nonterminal().await {
            Ok(tasks) => tasks,
            Err(e) => {
                tracing::warn!(error = %e, "scheduler sweep: task scan failed; skipping");
                return pending_waves;
            }
        };
        for mut task in tasks {
            match task.status {
                TaskStatus::Pending => {
                    pending_waves.insert(task.wave_id.clone());
                }
                TaskStatus::Dispatched => {
                    self.resume_dispatched(task).await;
                }
                // Must precede both terminal reconciliation and kind-based
                // timeout arms. Sub-wave parents have their own child-state
                // reconciler and never receive a worker deadline.
                TaskStatus::Running if task.spawn == "sub-wave" => {}
                TaskStatus::Running if task.kind == TaskKind::Terminal => {
                    self.reconcile_running_terminal(task).await;
                }
                TaskStatus::Running if task_has_running_liveness_deadline(&task) => {
                    let stamped = match self
                        .stamp_missing_running_liveness_deadline(&mut task)
                        .await
                    {
                        Ok(stamped) => stamped,
                        Err(e) => {
                            tracing::warn!(
                                task_id = %task.id,
                                error = %e,
                                "scheduler sweep: running liveness deadline stamp failed; next sweep retries"
                            );
                            continue;
                        }
                    };
                    if !stamped {
                        continue;
                    }
                    if task.status == TaskStatus::Running
                        && task_has_running_liveness_deadline(&task)
                        && task
                            .running_deadline_ms
                            .is_some_and(|deadline| now_ms() > deadline)
                    {
                        self.fail_running_liveness_timeout(task).await;
                    }
                }
                TaskStatus::Running => {}
                // §8 verifying arm (parked formulation): drive the
                // current gate attempt — op missing → submit,
                // non-terminal → single-flight `wait()` re-drive
                // (doubles as the parked-deadline watcher), terminal →
                // copy the outcome to the row. Spawned because a gate
                // watch can outlive the sweep by hours; dead-parked
                // enforcement itself is `sweep_parked`'s job above.
                TaskStatus::Verifying => {
                    let this = Arc::clone(self);
                    tokio::spawn(async move {
                        this.drive_gate(task).await;
                    });
                }
                TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled => {}
            }
        }
        self.reconcile_all_child_wave_tasks().await;
        pending_waves
    }

    async fn stamp_missing_running_liveness_deadline(&self, task: &mut Task) -> Result<bool> {
        if !task_has_running_liveness_deadline(task)
            || task.status != TaskStatus::Running
            || task.running_deadline_ms.is_some()
        {
            return Ok(true);
        }
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("scheduler requires a sqlite-backed Repo".into()))?;
        let mut tx = begin_immediate_tx(&pool).await?;
        let now = now_ms();
        let deadline = now.saturating_add(self.task_run_timeout_ms());
        let rows = task_stamp_missing_running_deadline_tx(&mut tx, &task.id, now, deadline).await?;
        let current = if rows == 0 {
            task_get_tx(&mut tx, &task.id).await?
        } else {
            None
        };
        tx.commit().await?;
        if rows > 0 {
            task.running_deadline_ms = Some(deadline);
            task.updated_at_ms = now;
        } else if let Some(current) = current {
            *task = current;
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    async fn fail_running_liveness_timeout(self: &Arc<Self>, task: Task) {
        let wave = match self.repo.wave_get(&task.wave_id).await {
            Ok(Some(wave)) => wave,
            Ok(None) => {
                tracing::warn!(
                    task_id = %task.id,
                    "scheduler sweep: running timeout task's wave row is gone; leaving row"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "scheduler sweep: running timeout wave_get failed"
                );
                return;
            }
        };

        let cleanup_card_id = self.worker_card_id_for_task(&task).await;

        match self
            .fail_task_liveness_timeout(
                &task,
                &wave,
                "worker exceeded the running liveness deadline",
                "[auto] worker liveness timeout",
                cleanup_card_id.as_deref(),
            )
            .await
        {
            Ok(true) => {
                self.sweep_timeout_worker_cleanups().await;
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "scheduler sweep: running timeout fail tx failed"
                );
            }
        }
    }

    async fn fail_task_liveness_timeout(
        &self,
        task: &Task,
        wave: &Wave,
        reason: &str,
        auto_message: &str,
        timeout_cleanup_card_id: Option<&str>,
    ) -> Result<bool> {
        let scope = EventScope::Wave {
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let task_id = task.id.clone();
        let wave_id = wave.id.clone();
        let reason = reason.to_string();
        let auto_message = auto_message.to_string();
        let timeout_cleanup_card_id = timeout_cleanup_card_id.map(str::to_string);
        let result = write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let now = now_ms();
                    let rows = task_fail_from_worker_tx(
                        tx,
                        &task_id,
                        wave_id.as_str(),
                        TaskReporter::Kernel,
                        "worker-timeout",
                        now,
                    )
                    .await?;
                    if rows == 0 {
                        return Err(race_lost_err());
                    }
                    if let Some(card_id) = timeout_cleanup_card_id.as_deref() {
                        mark_running_timeout_cleanup_tx(tx, card_id, &task_id, now).await?;
                    }
                    let mut events = vec![(
                        ActorId::KernelDispatcher,
                        scope.clone(),
                        Event::TaskFailed {
                            idempotency_key: task_id.clone(),
                            reason,
                            agent_message: None,
                        },
                    )];
                    if let Some(auto_events) = auto_transition_if_current_in_tx(
                        tx,
                        &wave_id,
                        WaveLifecycle::Working,
                        WaveLifecycle::Reviewing,
                        &ActorId::KernelDispatcher,
                        Some(auto_message),
                    )
                    .await?
                    {
                        events.extend(
                            auto_events
                                .into_iter()
                                .map(|event| (ActorId::KernelDispatcher, scope.clone(), event)),
                        );
                    }
                    Ok(((), events))
                })
            },
        )
        .await;

        match result {
            Ok(_) => Ok(true),
            Err(e) if is_race_lost(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn sweep_timeout_worker_cleanups(self: &Arc<Self>) {
        let Some(pool) = self.repo.sqlite_pool() else {
            return;
        };
        let worker_rows = match sqlx::query_as::<_, (String, String)>(
            r#"SELECT id, card_id
               FROM worker_sessions
               WHERE provider IN ('codex', 'claude')
                 AND card_id IS NOT NULL
                 AND json_extract(handle_state_json, '$.timeout_cleanup.requested_at_ms')
                     IS NOT NULL
               ORDER BY updated_at_ms ASC, id ASC"#,
        )
        .fetch_all(&pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "scheduler sweep: timed-out worker cleanup scan failed"
                );
                return;
            }
        };
        let Some(runtime) = self.operation_runtime.upgrade() else {
            if !worker_rows.is_empty() {
                tracing::warn!(
                    running_cleanup_count = worker_rows.len(),
                    "scheduler sweep: operation runtime dropped; cannot retry timed-out cleanup"
                );
            }
            return;
        };

        for (session_id, card_id) in worker_rows {
            let cleanup = TimeoutCleanupSession {
                session_id,
                card_id,
            };
            if let Err(e) = runtime.fail_running_worker_card(&cleanup.card_id).await {
                tracing::warn!(
                    session_id = %cleanup.session_id,
                    card_id = %cleanup.card_id,
                    error = %e,
                    "scheduler sweep: timed-out worker PTY/session cleanup failed; marker retained"
                );
                continue;
            }
            if let Err(e) = release_workspace_lease_for_card_repo(
                self.repo.as_ref(),
                &self.events,
                &cleanup.card_id,
            )
            .await
            {
                tracing::warn!(
                    session_id = %cleanup.session_id,
                    card_id = %cleanup.card_id,
                    error = %e,
                    "scheduler sweep: timed-out worker lease row release failed; marker retained"
                );
                continue;
            }
            if let Err(e) = self
                .clear_timeout_worker_cleanup_marker(&cleanup.session_id)
                .await
            {
                tracing::warn!(
                    session_id = %cleanup.session_id,
                    card_id = %cleanup.card_id,
                    error = %e,
                    "scheduler sweep: timed-out worker cleanup marker clear failed; next tick will retry"
                );
            }
        }
    }

    async fn clear_timeout_worker_cleanup_marker(&self, session_id: &str) -> Result<()> {
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("scheduler requires a sqlite-backed Repo".into()))?;
        sqlx::query(
            r#"UPDATE worker_sessions
               SET handle_state_json = json_remove(
                     COALESCE(handle_state_json, '{}'),
                     '$.timeout_cleanup'
                   ),
                   updated_at_ms = ?1
               WHERE id = ?2"#,
        )
        .bind(now_ms())
        .bind(session_id)
        .execute(&pool)
        .await?;
        Ok(())
    }

    async fn worker_card_id_for_task(&self, task: &Task) -> Option<String> {
        if let Some(card_id) = task.worker_card_id.as_ref() {
            return Some(card_id.clone());
        }
        let operation_kind = match task.kind {
            TaskKind::Codex => "codex-worker",
            TaskKind::Claude => "claude-worker",
            TaskKind::Terminal => return None,
        };
        self.operation_runtime
            .upgrade()?
            .find_by_kind_and_idempotency(operation_kind, &task.id)
            .await
            .ok()
            .flatten()
            .and_then(|op| op.target_id)
    }

    /// Sweep `dispatched` arm (§5.5/§8): the claim landed but the spawn
    /// outcome was never reconciled (crash between claim and op insert,
    /// between op success and the running stamp, or a lost completion).
    /// `drive_spawn` covers every sub-case via submit-dedupe + `wait()`
    /// + guarded reconcile writes.
    async fn resume_dispatched(self: &Arc<Self>, task: Task) {
        if !self.context_sweep_boot_done.load(Ordering::SeqCst) {
            tracing::debug!(
                task_id = %task.id,
                "scheduler sweep: dispatched task left untouched until context boot sweep completes"
            );
            return;
        }
        let Some(_inflight) = InflightGuard::acquire(&self.inflight, &task.id) else {
            return;
        };
        let wave = match self.repo.wave_get(&task.wave_id).await {
            Ok(Some(wave)) => wave,
            Ok(None) => {
                tracing::warn!(
                    task_id = %task.id,
                    "scheduler sweep: dispatched task's wave row is gone; leaving row"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(task_id = %task.id, error = %e, "scheduler sweep: wave_get failed");
                return;
            }
        };
        let _permit = match Arc::clone(&self.semaphore).acquire_owned().await {
            Ok(p) => p,
            Err(_) => return,
        };
        if let Err(e) = self.drive_spawn(&task, &wave).await {
            tracing::warn!(
                task_id = %task.id,
                error = %e,
                "scheduler sweep: dispatched-arm drive failed; next sweep retries"
            );
        }
    }

    /// Sweep `running`-terminal arm (§8/M2 downtime path): a `running`
    /// terminal task whose terminal row has a recorded exit gets the
    /// SAME guarded completion tx as the live exit hook. First writer
    /// wins via the status guard; live/sweep duplication is impossible.
    async fn reconcile_running_terminal(&self, task: Task) {
        let worker_card_id = match &task.worker_card_id {
            Some(id) => Some(id.clone()),
            // Crash between op success and the running stamp can leave
            // the card unstamped; recover it from the operation row
            // (idempotency-key convention, §2.2).
            None => match self.operation_runtime.upgrade() {
                Some(runtime) => runtime
                    .find_by_kind_and_idempotency("terminal-worker", &task.id)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|op| op.target_id),
                None => None,
            },
        };
        let Some(card_id) = worker_card_id else {
            tracing::debug!(
                task_id = %task.id,
                "scheduler sweep: running terminal task has no resolvable worker card; leaving row"
            );
            return;
        };
        let terminal = match self.repo.terminal_get_by_card(&card_id).await {
            Ok(Some(term)) => term,
            Ok(None) => {
                tracing::debug!(
                    task_id = %task.id,
                    card_id = %card_id,
                    "scheduler sweep: running terminal task has no terminal row; leaving row"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(task_id = %task.id, error = %e, "scheduler sweep: terminal_get_by_card failed");
                return;
            }
        };
        if terminal.exit_code.is_none() && !terminal.signal_killed {
            // Still running — nothing to reconcile (policy-free: no
            // liveness judgment beyond the persisted exit record).
            return;
        }
        if let Err(e) = complete_terminal_task(
            self.repo.as_ref(),
            &self.events,
            &self.write,
            &task.id,
            &task.wave_id,
            &card_id,
            terminal.exit_code,
            terminal.signal_killed,
        )
        .await
        {
            tracing::warn!(
                task_id = %task.id,
                error = %e,
                "scheduler sweep: terminal completion tx failed; next sweep retries"
            );
        }
    }

    /// Drive one `verifying` task's gate (issue #644 PR-C, §6.2 trigger
    /// 2 + the §8 verifying arms in the #653 parked formulation).
    /// Single-flight per task (`"gate:{task.id}"` — disjoint from the
    /// worker-spawn keyspace so a sweep gate drive never starves a
    /// spawn drive of the same task id, and vice versa); shared
    /// verbatim between the live pass (poked by `task.completed`) and
    /// the sweep arm. Deliberately does NOT hold the dispatch
    /// semaphore: the `wait()` below can span a multi-hour gate, and
    /// the real spawn work is bounded by the saga's own drive lease.
    async fn drive_gate(self: &Arc<Self>, task: Task) {
        let inflight_key = format!("gate:{}", task.id);
        let Some(_inflight) = InflightGuard::acquire(&self.inflight, &inflight_key) else {
            tracing::debug!(task_id = %task.id, "scheduler: gate drive already in flight");
            return;
        };
        let Some(runtime) = self.operation_runtime.upgrade() else {
            tracing::debug!(
                task_id = %task.id,
                "scheduler: operation runtime dropped; skipping gate drive"
            );
            return;
        };
        if let Err(e) = self.drive_gate_inner(&runtime, &task).await {
            tracing::warn!(
                task_id = %task.id,
                error = %e,
                "scheduler: gate drive failed; next trigger/sweep retries"
            );
        }
    }

    /// The §8 arm body. Resolution order (parked formulation):
    ///
    /// 1. `gate_attempt >= 1` and the op `"{task.id}#g{attempt}"`
    ///    exists → `wait()` it (terminal rows return immediately;
    ///    non-terminal ops get the single-flight re-drive — the one
    ///    public API that re-polls `drive()` — and a *parked* op's
    ///    wait-poll doubles as its deadline watcher, #653 §4.3), then
    ///    copy the outcome to the row iff it is still `verifying` at
    ///    that attempt (the live observer's one-tx completion normally
    ///    got there first and the guard misses — by design).
    /// 2. Op missing (or `gate_attempt == 0`, no attempt ever
    ///    prepared) → submit `#g{gate_attempt + 1}` and watch it the
    ///    same way. Racing submitters compute the same key and dedupe
    ///    on the operations unique index; the adapter's `prepare_tx`
    ///    bump admits exactly one op per attempt number.
    async fn drive_gate_inner(&self, runtime: &Arc<OperationRuntime>, task: &Task) -> Result<()> {
        if task.gate_attempt >= 1 {
            let key = gate_attempt_key(&task.id, task.gate_attempt);
            if let Some(op) = runtime
                .find_by_kind_and_idempotency(TASK_VERIFY_KIND, &key)
                .await?
            {
                let log_path = op
                    .spawn_artifacts
                    .as_ref()
                    .and_then(|a| a.log_path.clone())
                    .unwrap_or_default();
                let result = runtime.wait(&op.id).await?;
                return self
                    .reconcile_gate_outcome(task, task.gate_attempt, &log_path, result.outcome)
                    .await;
            }
        }
        let attempt = task.gate_attempt + 1;
        let payload = serde_json::to_value(TaskVerifyOperationPayload {
            actor: ActorId::KernelDispatcher,
            wave_id: task.wave_id.clone(),
            task_id: task.id.clone(),
            attempt,
        })?;
        let payload_hash = stable_payload_hash(&payload)?;
        let op_id = runtime
            .submit(
                TASK_VERIFY_KIND,
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(gate_attempt_key(&task.id, attempt)),
                    payload_hash,
                },
                payload,
            )
            .await?;
        let result = runtime.wait(&op_id).await?;
        self.reconcile_gate_outcome(task, attempt, "", result.outcome)
            .await
    }

    /// The consumer reconcile arm kept from #653 §6.2: "row
    /// `verifying`, op terminal → copy the outcome to the row". Needed
    /// because op-only terminal writes exist — boot recovery's
    /// `VerifyParked` Complete/Fail arms and `sweep_parked`'s
    /// enforcement write the *operation* without touching consumer
    /// tables (#653 §4.2). The copy runs the SAME one-tx body as the
    /// live observer (guarded flip + `task.gate_result` + lifecycle
    /// promotion), so first writer wins on the
    /// `status='verifying' AND gate_attempt=N` guard and duplication
    /// is impossible.
    ///
    /// Outcome mapping: op succeeded → its result IS the recorded
    /// `GateVerdict` (the observer/recovery spliced it into
    /// `tx_output.result`); op failed `parked_deadline` →
    /// `gate-timeout`; any other failure / stuck / unparseable result
    /// → `gate-infra`. A benignly-failed attempt (lost `prepare_tx`
    /// bump — its Conflict error fails the op) can never mis-fail the
    /// row: losing the bump means the row's `gate_attempt` never
    /// reached this op's attempt number, so the in-tx guard misses.
    async fn reconcile_gate_outcome(
        &self,
        task: &Task,
        attempt: i64,
        log_path: &str,
        outcome: OperationOutcome,
    ) -> Result<()> {
        // PR #685 review F4 — whether the op terminal-FAILED (vs
        // succeeded with a recorded verdict). Only a failed/stuck op is
        // eligible for the pre-bump fallback below: an op that reached
        // a verdict necessarily ran `prepare_tx`'s bump first, so its
        // eq-attempt guard is the only correct one.
        let op_terminal_failed = matches!(
            outcome,
            OperationOutcome::Failed { .. } | OperationOutcome::Stuck { .. }
        );
        let verdict = match outcome {
            OperationOutcome::Succeeded { result }
            | OperationOutcome::SucceededViaCollision { result, .. } => {
                match serde_json::from_value::<GateVerdict>(result) {
                    Ok(verdict) => verdict,
                    Err(e) => GateVerdict {
                        passed: false,
                        status_detail: Some("gate-infra".into()),
                        failing_step: None,
                        exit_code: None,
                        log_tail: format!("gate op result unparseable: {e}"),
                        log_path: log_path.to_string(),
                        attempt,
                    },
                }
            }
            OperationOutcome::Failed {
                last_error,
                last_error_class,
                ..
            } => {
                let status_detail = if last_error_class.as_deref() == Some("parked_deadline") {
                    "gate-timeout"
                } else {
                    "gate-infra"
                };
                GateVerdict {
                    passed: false,
                    status_detail: Some(status_detail.into()),
                    failing_step: None,
                    exit_code: None,
                    log_tail: last_error,
                    log_path: log_path.to_string(),
                    attempt,
                }
            }
            OperationOutcome::Stuck { reason, .. } => GateVerdict {
                passed: false,
                status_detail: Some("gate-infra".into()),
                failing_step: None,
                exit_code: None,
                log_tail: reason,
                log_path: log_path.to_string(),
                attempt,
            },
        };
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("scheduler requires a sqlite-backed Repo".into()))?;
        let Some(wave) = self.repo.wave_get(&task.wave_id).await? else {
            tracing::debug!(task_id = %task.id, "scheduler: gate task's wave row is gone");
            return Ok(());
        };
        let rctx = GateResultCtx {
            task_id: task.id.clone(),
            wave_id: wave.id.clone(),
            cove_id: wave.cove_id.clone(),
        };
        let mut tx = begin_immediate_tx(&pool).await?;
        let mut envelopes = apply_gate_result_in_tx(&mut tx, &rctx, &verdict).await?;
        if envelopes.is_empty() && op_terminal_failed && verdict.attempt >= 1 {
            // PR #685 review F4 — the pre-bump failure arm. A client
            // error in `prepare_tx` BEFORE the guarded bump (wave row
            // gone → Conflict) terminal-fails op `#gN` while the row
            // stays `verifying@N-1`; every later drive recomputes the
            // same key, dedupes onto the dead op, and the eq-attempt
            // guard above misses forever — a permanent loop with no
            // outcome, no event, and no operator escape. Flip the row
            // at its pre-bump attempt instead: the failed op can never
            // bump it, no second op for attempt N can exist (operations
            // unique index), and a row that DID reach attempt N makes
            // this relaxed guard miss, so a benignly-failed attempt
            // still cannot mis-fail a row another op owns.
            envelopes = crate::operation::task_verify_adapter::apply_gate_result_with_guard_in_tx(
                &mut tx,
                &rctx,
                &verdict,
                verdict.attempt - 1,
            )
            .await?;
        }
        if envelopes.is_empty() {
            // Guard miss: the live observer's tx (or a superseding
            // attempt) already moved the row. Nothing was written.
            tx.rollback().await?;
            return Ok(());
        }
        tx.commit().await?;
        for envelope in envelopes {
            self.events.emit_envelope(envelope);
        }
        Ok(())
    }
}

/// Terminal-exit completion bundle (issue #644 M2, live path). Threaded
/// from the dispatcher construction site into
/// [`crate::terminal_renderer::TerminalRendererRegistry`] so the
/// attach-reader exit branch can resolve terminal → card → payload
/// `idempotency_key` and run the shared guarded completion tx. Carries
/// the same set the dispatcher's `Inner` owns: repo + EventBus + role
/// caches (`WriteContext`).
pub struct TerminalTaskHook {
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
}

impl TerminalTaskHook {
    pub fn new(repo: Arc<dyn Repo>, events: EventBus, write: WriteContext) -> Arc<Self> {
        Arc::new(Self {
            repo,
            events,
            write,
        })
    }

    /// Live exit path. Resolves the exited terminal to a plan-task row
    /// (terminal → card → payload `idempotency_key` — stamped at worker
    /// create time, so this works even when the exit beats the
    /// scheduler's `worker_card_id` stamp) and, if one exists, runs the
    /// shared guarded completion tx. The payload walk only FINDS the
    /// candidate row; ownership is proven inside the tx against the
    /// worker operation's immutable target card (round-4 review F2 —
    /// card payloads are patchable, so they are not proof). Terminals
    /// with no task row (user terminals, legacy `calm.task.dispatch`
    /// workers, spec terminals) no-op here.
    pub async fn on_terminal_exit(
        &self,
        terminal_id: &str,
        exit_code: Option<i32>,
        signal_killed: bool,
    ) {
        let card_id = match self.repo.terminal_get(terminal_id).await {
            Ok(Some(term)) => term.card_id,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(terminal_id, error = %e, "terminal task hook: terminal_get failed");
                return;
            }
        };
        let card = match self.repo.card_get(card_id.as_str()).await {
            Ok(Some(card)) => card,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(terminal_id, error = %e, "terminal task hook: card_get failed");
                return;
            }
        };
        let Some(task_id) = card
            .payload
            .get("idempotency_key")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return;
        };
        let task = match self.repo.task_get(&task_id).await {
            Ok(Some(task)) => task,
            Ok(None) => return, // legacy dispatch key — no plan row
            Err(e) => {
                tracing::warn!(terminal_id, error = %e, "terminal task hook: task_get failed");
                return;
            }
        };
        if task.status.is_terminal() {
            return;
        }
        // Review F1: only TERMINAL-kind tasks are mechanically
        // reconcilable from a PTY exit code. Codex worker cards are
        // also terminal-row-backed and carry the task id in their
        // payload `idempotency_key`, but a codex PTY exiting 0 says
        // nothing about the task — completion must come from the
        // worker's `calm.task.complete` report (mirrors the sweep's
        // kind-gated running arm).
        if task.kind != TaskKind::Terminal {
            return;
        }
        if let Err(e) = complete_terminal_task(
            self.repo.as_ref(),
            &self.events,
            &self.write,
            &task.id,
            &task.wave_id,
            card_id.as_str(),
            exit_code,
            signal_killed,
        )
        .await
        {
            tracing::warn!(
                terminal_id,
                task_id = %task.id,
                error = %e,
                "terminal task hook: completion tx failed; the sweep's running-terminal arm retries"
            );
        }
    }
}

/// The ONE guarded terminal-completion function (issue #644 M2) — both
/// the live attach-reader exit hook and the sweep's running-terminal
/// arm run exactly this tx; first writer wins via the
/// `status IN ('dispatched','running')` guard, the second no-ops.
///
/// Exit 0 → `task.completed`; non-zero / signal / synthetic `-1` →
/// `task.failed`. Every event uses actor `ActorId::KernelDispatcher`
/// (so `is_spec_verdict_event` never classifies it as a spec verdict)
/// at wave scope, stamps `worker_card_id` via COALESCE, and carries the
/// same `Working → Reviewing` promotion as a worker self-report (§3).
#[allow(clippy::too_many_arguments)]
pub async fn complete_terminal_task(
    repo: &dyn Repo,
    events: &EventBus,
    write: &WriteContext,
    task_id: &str,
    wave_id: &str,
    worker_card_id: &str,
    exit_code: Option<i32>,
    signal_killed: bool,
) -> Result<()> {
    let Some(wave) = repo.wave_get(wave_id).await? else {
        return Ok(());
    };
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let success = !signal_killed && exit_code == Some(0);
    let task_id = task_id.to_string();
    let wave_id_str = wave_id.to_string();
    let wave_id_typed = wave.id.clone();
    let worker_card_id = worker_card_id.to_string();
    let result = write_with_actor_events_typed::<(), _>(repo, None, events, write, move |tx| {
        Box::pin(async move {
            let now = now_ms();
            // Round-4 review F2: the live hook resolves the task from
            // the exiting card's PAYLOAD `idempotency_key`, which is
            // mutable via `PATCH /api/cards/{id}` — so it is NOT proof
            // of ownership. Prove it against the immutable worker-spawn
            // operation target instead: only the card the op actually
            // created may flip an UNSTAMPED row. Stamped rows are still
            // guarded by `worker_card_id = card` (the sweep arm's
            // row-stamp resolution rides that side); a forged-payload
            // card fails both sides → 0 rows → no event.
            let owns_key =
                crate::db::sqlite::worker_op_targets_card_tx(tx, &task_id, &worker_card_id).await?;
            let reporter = TaskReporter::Card {
                card_id: worker_card_id.as_str(),
                owns_key,
            };
            // Issue #644 PR-C (§3): a gated terminal task's clean exit
            // is still a self-report — the row goes to `verifying` and
            // the `Working → Reviewing` promotion is suppressed (the
            // gate-result tx promotes instead).
            let mut suppress_promotion = false;
            let (rows, event) = if success {
                let flip =
                    task_report_success_from_worker_tx(tx, &task_id, &wave_id_str, reporter, now)
                        .await?;
                if flip == SuccessReportFlip::Verifying {
                    suppress_promotion = true;
                }
                (
                    if flip == SuccessReportFlip::None {
                        0
                    } else {
                        1
                    },
                    Event::TaskCompleted {
                        idempotency_key: task_id.clone(),
                        result: json!({ "exit_code": 0, "source": "terminal-exit" }),
                        artifacts: Vec::new(),
                        agent_message: None,
                    },
                )
            } else {
                let reason = if signal_killed {
                    "terminal worker killed by signal".to_string()
                } else {
                    match exit_code {
                        Some(-1) => {
                            "terminal worker exited while the kernel was down (outcome unknown)"
                                .to_string()
                        }
                        Some(code) => format!("terminal worker exited with code {code}"),
                        None => "terminal worker exited without an exit code".to_string(),
                    }
                };
                (
                    task_fail_from_worker_tx(
                        tx,
                        &task_id,
                        &wave_id_str,
                        reporter,
                        "worker-reported",
                        now,
                    )
                    .await?,
                    Event::TaskFailed {
                        idempotency_key: task_id.clone(),
                        reason,
                        agent_message: None,
                    },
                )
            };
            if rows == 0 {
                // First writer already won (live hook vs sweep, or a
                // belt-and-suspenders worker self-report) — no row
                // change, no event.
                return Err(race_lost_err());
            }
            let mut events = vec![(ActorId::KernelDispatcher, scope.clone(), event)];
            if !suppress_promotion
                && let Some(auto_events) = auto_transition_if_current_in_tx(
                    tx,
                    &wave_id_typed,
                    WaveLifecycle::Working,
                    WaveLifecycle::Reviewing,
                    &ActorId::KernelDispatcher,
                    Some("[auto] terminal task finished".to_string()),
                )
                .await?
            {
                events.extend(
                    auto_events
                        .into_iter()
                        .map(|event| (ActorId::KernelDispatcher, scope.clone(), event)),
                );
            }
            Ok(((), events))
        })
    })
    .await;
    match result {
        Ok(_) => Ok(()),
        Err(e) if is_race_lost(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests;
