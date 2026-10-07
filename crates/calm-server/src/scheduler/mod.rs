//! Kernel task scheduler: the only component that moves plan tasks
//! `pending → dispatched → running`. Policy-free: it never re-runs a `failed` task,
//! never reorders beyond `(priority DESC, created_at ASC, key ASC)`, never edits the plan.

mod git_delivery;
mod running_worker;
mod worker_failure;
mod worker_liveness;
#[cfg(feature = "fixtures")]
pub use running_worker::LivenessFailTestHook;
use running_worker::RunningWorkerFailure;
pub(crate) use running_worker::WorkerCleanupReason;
pub use running_worker::{
    PLANNER_CANCELED, WORKER_IDLE_PROBE_TIMEOUT, WORKER_IDLE_TURN_GRACE, WORKER_TURN_ENDED,
    WorkerIdleClock, WorkerIdleWake,
};
pub(crate) use worker_failure::{fail_tasks_for_deleted_card_tx, fail_worker_task_tx};
pub(crate) use worker_liveness::LivenessExpiry;
pub use worker_liveness::WorkerLiveness;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use serde_json::{Value, json};
use tokio::sync::{Notify, Semaphore};

use crate::db::sqlite::{
    RunningLivenessFacts, SuccessReportFlip, TaskReporter, begin_immediate_tx,
    status_detail_with_reason, task_claim_pending_tx, task_fail_from_worker_tx, task_get_tx,
    task_mark_running_tx, task_mark_sub_track_running_tx, task_report_success_from_worker_tx,
    task_running_liveness_tx, task_stamp_missing_running_liveness_tx, tasks_by_track_tx,
    track_find_tx,
};
use crate::db::{Repo, RouteRepo, write_with_actor_events_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::{ActorId, TrackId};
use crate::model::{Task, TaskKind, TaskStart, TaskStatus, Track, new_id, now_ms};
use crate::operation::child_track_adapter::{CHILD_TRACK_KIND, ChildTrackOperationPayload};
use crate::operation::claude_adapter::ClaudeWorkerOperationPayload;
use crate::operation::codex_adapter::CodexWorkerOperationPayload;
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use crate::operation::planner_start_fence::CardStartFence;
use crate::operation::task_verify_adapter::{
    GateResultCtx, TASK_VERIFY_KIND, TaskVerifyOperationPayload, apply_gate_result_in_tx,
    gate_attempt_key,
};
use crate::operation::terminal_adapter::TerminalWorkerOperationPayload;
use crate::operation::workspace_lease::{
    ReleaseDelivery, release_workspace_lease_for_card_repo, release_workspace_lease_for_card_tx,
};
use crate::operation::{OperationKey, OperationOutcome, OperationRuntime, Tx};
use crate::per_card_lock::PerCardLocks;
use crate::routes::idempotency_key::stable_payload_hash;
use crate::state::WriteContext;
use crate::task_context::{ContextMetrics, TaskContextMonitor, context_ref};

/// Default reconcile-tick period (liveness backstop).
pub const DEFAULT_RECONCILE_SECS: u64 = 300;

/// Sentinel: a guarded flip affected 0 rows because another writer won; carried through
/// `CalmError::Conflict` so the eventized-write helper rolls back without persisting events.
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

/// What [`mark_running_timeout_cleanup_tx`] did: the sessions it marked for the sweep's reap, and
/// the lease events of a release it made when it marked none.
pub(crate) struct TimeoutCleanupMark {
    pub(crate) marked: u64,
    pub(crate) released: Vec<(ActorId, EventScope, Event)>,
}

/// Mark the card's live worker session for the sweep's reap (`sweep_timeout_worker_cleanups`),
/// after the caller flipped the task terminal in `tx`. When no live session is left to mark (a
/// Claude PTY that died is `exited`), no worker is left to kill: the lease is released here, in
/// the caller's transaction, and the attempt committed as its terminal status says (#1830 S2 D7).
pub(crate) async fn mark_running_timeout_cleanup_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
    task_id: &str,
    now: i64,
    reason: WorkerCleanupReason,
) -> Result<TimeoutCleanupMark> {
    let marker = serde_json::to_string(&json!({
        "task_id": task_id,
        "requested_at_ms": now,
        "reason": reason.as_str(),
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
    let released = if rows == 0 {
        release_workspace_lease_for_card_tx(tx, card_id, ReleaseDelivery::CommitAsTaskEnded).await?
    } else {
        Vec::new()
    };
    Ok(TimeoutCleanupMark {
        marked: rows,
        released,
    })
}

/// Ready-set computation over one track's plan rows (already in scheduler order): the tasks
/// [`crate::db::sqlite::checkout_admission`] admits under `occupancy` (#1830 S2 D5, #1917). The
/// claim tx re-runs that rule, so a claim that fails does not hold the others.
pub fn compute_ready(tasks: &[Task], occupancy: crate::db::sqlite::CheckoutOccupancy) -> Vec<Task> {
    crate::db::sqlite::checkout_admission(tasks, occupancy)
        .into_iter()
        .filter(|(_, wait)| wait.is_none())
        .map(|(task, _)| task.clone())
        .collect()
}

/// #2058 D5: fetch the upstream a catch-up of `track` starts from: that of the branch of the
/// checkout its worker runs in and its prepare reads, `agent_cwd()` (#2112: the track worktree,
/// never the main checkout). Bounded and fail-soft: a failed fetch leaves no fetch receipt, which
/// prepare refuses.
async fn refresh_catch_up_upstream(track: &Track) {
    let checkout = std::path::Path::new(track.workspace.agent_cwd());
    crate::operation::workspace_lease::upstream_fetch::refresh_upstream(checkout).await;
}

/// Build the worker-operation payload as a pure function of the frozen task row, so a
/// post-crash resubmit idempotency-matches the original instead of conflicting on payload hash.
pub fn build_worker_payload(task: &Task) -> Result<(&'static str, Value)> {
    match task.kind {
        TaskKind::Codex => {
            let payload = serde_json::to_value(CodexWorkerOperationPayload {
                actor: ActorId::KernelDispatcher,
                track_id: task.track_id.clone(),
                idempotency_key: task.id.clone(),
                goal: task.goal.clone(),
                // The lease path from `prepare_tx` is the authoritative cwd; serializing `task.cwd`
                // would change `stable_payload_hash` for in-flight tasks created by older builds.
                cwd: None,
                context: serde_json::from_str(&task.context_json).unwrap_or(Value::Null),
                acceptance_criteria: task.acceptance_criteria.clone(),
            })?;
            Ok(("codex-worker", payload))
        }
        TaskKind::Claude => {
            let payload = serde_json::to_value(ClaudeWorkerOperationPayload {
                actor: ActorId::KernelDispatcher,
                track_id: task.track_id.clone(),
                idempotency_key: task.id.clone(),
                goal: task.goal.clone(),
                // Same as codex-worker: keep `task.cwd` out of the payload hash.
                cwd: None,
                context: serde_json::from_str(&task.context_json).unwrap_or(Value::Null),
                acceptance_criteria: task.acceptance_criteria.clone(),
            })?;
            Ok(("claude-worker", payload))
        }
        TaskKind::Terminal => {
            let payload = serde_json::to_value(TerminalWorkerOperationPayload {
                actor: ActorId::KernelDispatcher,
                track_id: task.track_id.clone(),
                idempotency_key: task.id.clone(),
                cmd: task.goal.clone(),
                // Row value AS-IS: materializing `default_cwd()` here would make `stable_payload_hash`
                // depend on process env, so a restart under a different HOME would fail the task.
                cwd: task.cwd.clone(),
            })?;
            Ok(("terminal-worker", payload))
        }
    }
}

/// Child creation identity is a pure function of the post-claim row. Parent
/// area/cwd are copied by the adapter inside its IMMEDIATE transaction so a
/// later track patch cannot change this operation's payload hash.
pub fn build_child_track_payload(task: &Task) -> Result<Value> {
    Ok(serde_json::to_value(ChildTrackOperationPayload {
        task_id: task.id.clone(),
        parent_track_id: task.track_id.clone(),
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

pub(crate) fn task_has_running_liveness_deadline(task: &Task) -> bool {
    task.spawn != "sub-wave" && matches!(task.kind, TaskKind::Codex | TaskKind::Claude)
}

fn duration_ms_i64(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// RAII guard for the per-task single-flight map. Losing a slot is always safe — the
/// holder performs the same guarded writes; this just avoids duplicate `wait()` polling.
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
    parent_track_id: String,
    parent_area_id: String,
    child_track_id: String,
    child_exists: bool,
    child_closed_at: Option<i64>,
    gate_json: Option<String>,
    inflight_count: i64,
    pending_count: i64,
}

/// Final child-success compare-and-set. The snapshot that selected the
/// success arm is advisory; this statement is the authority and rechecks that
/// the child is closed and quiescent in the writer transaction.
async fn guarded_child_success_flip_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
    child_track_id: &str,
    target_status: &str,
    now: i64,
    finished_at_ms: Option<i64>,
) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE tasks SET status=?1,status_detail=NULL,worker_card_id=NULL,\
                running_deadline_ms=NULL,updated_at_ms=?2,finished_at_ms=?3 \
           WHERE id=?4 AND child_track_id=?5 \
             AND status IN ('dispatched','running') \
             AND EXISTS(SELECT 1 FROM tracks child \
                WHERE child.id=?5 AND child.closed_at IS NOT NULL) \
             AND NOT EXISTS(SELECT 1 FROM current_tasks ct WHERE ct.track_id=?5 \
                AND ct.status IN ('pending','dispatched','running','verifying'))",
    )
    .bind(target_status)
    .bind(now)
    .bind(finished_at_ms)
    .bind(task_id)
    .bind(child_track_id)
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// Final pending-incomplete compare-and-set, kept separate from the success flip.
async fn guarded_child_incomplete_flip_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
    parent_track_id: &str,
    child_track_id: &str,
    now: i64,
) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE tasks SET status='failed',status_detail='child-track-incomplete',\
                worker_card_id=NULL,running_deadline_ms=NULL,updated_at_ms=?1,finished_at_ms=?1 \
           WHERE id=?2 AND track_id=?3 AND child_track_id=?4 \
             AND status IN ('dispatched','running') \
             AND EXISTS(SELECT 1 FROM tracks child \
                WHERE child.id=?4 AND child.closed_at IS NOT NULL) \
             AND NOT EXISTS(SELECT 1 FROM current_tasks ct WHERE ct.track_id=?4 \
                AND ct.status IN ('dispatched','running','verifying')) \
             AND EXISTS(SELECT 1 FROM current_tasks ct WHERE ct.track_id=?4 AND ct.status='pending')",
    )
    .bind(now)
    .bind(task_id)
    .bind(parent_track_id)
    .bind(child_track_id)
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// Final deleted-child compare-and-set. The snapshot that selected it is
/// advisory; this statement rechecks that the child row is gone.
async fn guarded_child_deleted_flip_tx(
    tx: &mut Tx<'_>,
    task_id: &str,
    parent_track_id: &str,
    child_track_id: &str,
    now: i64,
) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE tasks SET status='failed',status_detail='child-track-deleted',worker_card_id=NULL,\
                running_deadline_ms=NULL,updated_at_ms=?1,finished_at_ms=?1 \
           WHERE id=?2 AND track_id=?3 AND child_track_id=?4 \
             AND status IN ('dispatched','running') \
             AND NOT EXISTS(SELECT 1 FROM tracks child WHERE child.id=?4)",
    )
    .bind(now)
    .bind(task_id)
    .bind(parent_track_id)
    .bind(child_track_id)
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub async fn guarded_child_success_flip_for_test(
    tx: &mut Tx<'_>,
    task_id: &str,
    child_track_id: &str,
) -> Result<u64> {
    let now = now_ms();
    guarded_child_success_flip_tx(tx, task_id, child_track_id, "done", now, Some(now)).await
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub async fn guarded_child_incomplete_flip_for_test(
    tx: &mut Tx<'_>,
    task_id: &str,
    parent_track_id: &str,
    child_track_id: &str,
) -> Result<u64> {
    guarded_child_incomplete_flip_tx(tx, task_id, parent_track_id, child_track_id, now_ms()).await
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub async fn guarded_child_deleted_flip_for_test(
    tx: &mut Tx<'_>,
    task_id: &str,
    parent_track_id: &str,
    child_track_id: &str,
) -> Result<u64> {
    guarded_child_deleted_flip_tx(tx, task_id, parent_track_id, child_track_id, now_ms()).await
}

pub struct Scheduler {
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    /// Same `Weak` discipline as the dispatcher's `Inner` — the
    /// scheduler must not keep AppState resources alive after shutdown.
    operation_runtime: Weak<OperationRuntime>,
    /// The process's one `planner_recovery_locks` map, shared with `RouteState` from boot: the
    /// child-track bootstrap starts the child's Planner under that card's `CardStartFence`.
    pub(crate) planner_recovery_locks: PerCardLocks,
    /// The dispatcher's global spawn semaphore: caps total cross-track spawn work.
    semaphore: Arc<Semaphore>,
    /// The running-worker windows: the cap is stamped per task, the idle window applies live.
    worker_liveness: WorkerLiveness,
    /// When this scheduler was built; no worker counts as idle before a full window past it.
    liveness_floor_ms: AtomicI64,
    /// Live recheck behind the sweep's idle arm (#1785).
    worker_idle: WorkerIdleWake,
    /// Per-task single-flight for spawned idle rechecks.
    idle_checks: Arc<DashMap<String, ()>>,
    /// Where a re-submitted git delivery's forge result files go (one value with the MCP context's).
    gate_logs_dir: std::path::PathBuf,
    /// Per-track single-flight: exactly the push-locks pattern.
    track_locks: DashMap<TrackId, Arc<tokio::sync::Mutex<()>>>,
    /// Dirty flags — a trigger arriving mid-pass marks dirty and the
    /// lock holder loops once more, so no envelope is ever lost to "a
    /// pass was already running".
    track_dirty: DashMap<TrackId, Arc<AtomicBool>>,
    /// Per-task single-flight for submit/wait drives (live + sweep).
    inflight: Arc<DashMap<String, ()>>,
    /// Boot-order gate for the backstop sweeps: the reconcile tick may fire before boot
    /// recovery, so `sweep_all` no-ops until `sweep_boot` completes.
    boot_sweep_done: AtomicBool,
    /// Dispatched recovery must not start until the boot context sweep has persisted every
    /// material verdict.
    context_sweep_boot_done: AtomicBool,
    context_metrics: Arc<ContextMetrics>,
    claim_fence_test_hook: std::sync::Mutex<Option<ClaimFenceTestHook>>,
    post_claim_drive_test_hook: std::sync::Mutex<Option<PostClaimDriveTestHook>>,
    #[cfg(feature = "fixtures")]
    reconcile_child_reopen_after_snapshot_test_hook: AtomicBool,
    #[cfg(feature = "fixtures")]
    poke_count: std::sync::atomic::AtomicUsize,
    #[cfg(feature = "fixtures")]
    liveness_fail_test_hook: std::sync::Mutex<Option<LivenessFailTestHook>>,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        operation_runtime: Weak<OperationRuntime>,
        planner_recovery_locks: PerCardLocks,
        semaphore: Arc<Semaphore>,
        gate_logs_dir: std::path::PathBuf,
        worker_liveness: WorkerLiveness,
        worker_idle: WorkerIdleWake,
    ) -> Arc<Self> {
        Arc::new(Self {
            repo,
            events,
            write,
            operation_runtime,
            planner_recovery_locks,
            semaphore,
            worker_liveness,
            liveness_floor_ms: AtomicI64::new(now_ms()),
            worker_idle,
            idle_checks: Self::new_idle_checks(),
            gate_logs_dir,
            track_locks: DashMap::new(),
            track_dirty: DashMap::new(),
            inflight: Arc::new(DashMap::new()),
            boot_sweep_done: AtomicBool::new(false),
            context_sweep_boot_done: AtomicBool::new(false),
            context_metrics: Arc::new(ContextMetrics::default()),
            claim_fence_test_hook: std::sync::Mutex::new(None),
            post_claim_drive_test_hook: std::sync::Mutex::new(None),
            #[cfg(feature = "fixtures")]
            reconcile_child_reopen_after_snapshot_test_hook: AtomicBool::new(false),
            #[cfg(feature = "fixtures")]
            poke_count: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(feature = "fixtures")]
            liveness_fail_test_hook: std::sync::Mutex::new(None),
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
    pub fn poke_count_for_test(&self) -> usize {
        self.poke_count.load(Ordering::SeqCst)
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

    /// Fixtures only: move the boot floor back, as if this scheduler had booted at `floor_ms`.
    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub fn set_liveness_floor_for_test(&self, floor_ms: i64) {
        self.liveness_floor_ms.store(floor_ms, Ordering::SeqCst);
    }

    /// Fire-and-forget trigger: schedule the track on a fresh task. Used
    /// by the dispatcher's envelope arms.
    pub fn poke(self: &Arc<Self>, track_id: TrackId) {
        #[cfg(feature = "fixtures")]
        self.poke_count.fetch_add(1, Ordering::SeqCst);
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.schedule_track(track_id).await;
        });
    }

    /// Low-latency child close/deletion trigger. The event carries only a
    /// hint; the guarded IMMEDIATE transaction below rereads current DB state.
    pub fn reconcile_child_track(self: &Arc<Self>, child_track_id: TrackId) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = this
                .reconcile_child_track_task(child_track_id.as_str())
                .await
            {
                tracing::warn!(%error, %child_track_id, "scheduler: live child-track reconcile failed; sweep will retry");
            }
        });
    }

    async fn reconcile_all_child_track_tasks(&self) {
        let Some(pool) = self.repo.sqlite_pool() else {
            return;
        };
        let child_ids: Vec<String> = match sqlx::query_scalar(
            "SELECT child_track_id FROM tasks WHERE child_track_id IS NOT NULL \
             AND status IN ('dispatched','running') ORDER BY created_at_ms",
        )
        .fetch_all(&pool)
        .await
        {
            Ok(ids) => ids,
            Err(error) => {
                tracing::warn!(%error, "scheduler sweep: child-track scan failed");
                return;
            }
        };
        for child_id in child_ids {
            if let Err(error) = self.reconcile_child_track_task(&child_id).await {
                tracing::warn!(%error, child_track_id = %child_id, "scheduler sweep: child-track reconcile failed");
            }
        }
    }

    async fn reconcile_child_track_task(&self, child_track_id: &str) -> Result<()> {
        let child_track_id = child_track_id.to_string();
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
                        r#"SELECT t.id AS task_id, t.track_id AS parent_track_id,
                                  parent.area_id AS parent_area_id,
                                  t.child_track_id AS child_track_id,
                                  child.id IS NOT NULL AS child_exists,
                                  child.closed_at AS child_closed_at,
                                  t.gate_json AS gate_json,
                                  (SELECT count(*) FROM current_tasks ct
                                    WHERE ct.track_id=t.child_track_id
                                      AND ct.status IN ('dispatched','running','verifying')) AS inflight_count,
                                  (SELECT count(*) FROM current_tasks ct
                                    WHERE ct.track_id=t.child_track_id
                                      AND ct.status='pending') AS pending_count
                             FROM tasks t
                             JOIN tracks parent ON parent.id=t.track_id
                        LEFT JOIN tracks child ON child.id=t.child_track_id
                            WHERE t.child_track_id=?1
                              AND t.status IN ('dispatched','running')"#,
                    )
                    .bind(&child_track_id)
                    .fetch_optional(&mut **tx)
                    .await?;
                    let Some(snapshot) = snapshot else {
                        return Err(race_lost_err());
                    };
                    #[cfg(feature = "fixtures")]
                    if reopen_child_after_snapshot {
                        sqlx::query("UPDATE tracks SET closed_at=NULL WHERE id=?1")
                            .bind(&snapshot.child_track_id)
                            .execute(&mut **tx)
                            .await?;
                    }
                    let scope = EventScope::Track {
                        track: TrackId::from(snapshot.parent_track_id.clone()),
                        area: snapshot.parent_area_id.clone().into(),
                    };
                    let now = now_ms();
                    let child_closed = snapshot.child_exists && snapshot.child_closed_at.is_some();

                    if child_closed
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
                            &snapshot.child_track_id,
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
                                "source": "child-track",
                                "child_track_id": snapshot.child_track_id,
                            }),
                            artifacts: vec![],
                            agent_message: None,
                        };
                        return Ok(((), vec![(ActorId::KernelDispatcher, scope, event)]));
                    }

                    if child_closed
                        && snapshot.inflight_count == 0
                        && snapshot.pending_count > 0
                    {
                        let changed = guarded_child_incomplete_flip_tx(
                            tx,
                            &snapshot.task_id,
                            &snapshot.parent_track_id,
                            &snapshot.child_track_id,
                            now,
                        )
                        .await?;
                        if changed == 0 {
                            return Err(race_lost_err());
                        }
                        let event = Event::TaskFailed {
                            idempotency_key: snapshot.task_id,
                            reason: format!(
                                "child-track-incomplete: {} pending task(s) remain",
                                snapshot.pending_count
                            ),
                            details: None,
                            agent_message: None,
                        };
                        return Ok(((), vec![(ActorId::KernelDispatcher, scope, event)]));
                    }

                    if snapshot.child_exists {
                        return Err(race_lost_err());
                    }
                    let changed = guarded_child_deleted_flip_tx(
                        tx,
                        &snapshot.task_id,
                        &snapshot.parent_track_id,
                        &snapshot.child_track_id,
                        now,
                    )
                    .await?;
                    if changed == 0 {
                        return Err(race_lost_err());
                    }
                    let event = Event::TaskFailed {
                        idempotency_key: snapshot.task_id,
                        reason: "child-track-deleted: child track was deleted".into(),
                        details: None,
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
    pub async fn reconcile_child_track_for_test(&self, child_track_id: &str) -> Result<()> {
        self.reconcile_child_track_task(child_track_id).await
    }

    /// Run scheduling passes for one track until quiescent. Per-track
    /// mutex + dirty flag: concurrent callers collapse into the lock
    /// holder's loop.
    pub async fn schedule_track(self: &Arc<Self>, track_id: TrackId) {
        let dirty = self
            .track_dirty
            .entry(track_id.clone())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone();
        dirty.store(true, Ordering::SeqCst);
        // IMPORTANT: do NOT bind the DashMap Entry to a `let` — the
        // shard guard must drop at this statement's `;` before the
        // `.await` below (same hazard as the dispatcher's push locks).
        let lock = self
            .track_locks
            .entry(track_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        while dirty.swap(false, Ordering::SeqCst) {
            if let Err(e) = self.schedule_pass(&track_id).await {
                tracing::warn!(
                    track_id = %track_id,
                    error = %e,
                    "scheduler: scheduling pass failed; will retry on next trigger/sweep"
                );
            }
        }
    }

    /// One pass under the track lock: open gate → ready set → dispatch each ready task
    /// sequentially.
    async fn schedule_pass(self: &Arc<Self>, track_id: &TrackId) -> Result<()> {
        let Some(track) = self.repo.track_get(track_id.as_str()).await? else {
            return Ok(());
        };
        let tasks = self.repo.tasks_by_track(track_id.as_str()).await?;
        // Before the open gate: a delivery settles (and wakes the planner) on a closed track too.
        self.resume_git_deliveries(track_id.as_str()).await?;
        // Drive each `verifying` task's gate, fire-and-forget: a gate can run for hours and
        // must never block the track lock. Deliberately BEFORE the open gate: it scopes NEW
        // claims only.
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
        if !track.is_open() {
            tracing::debug!(
                track_id = %track_id,
                "scheduler: track is closed; skipping pass"
            );
            return Ok(());
        }
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("scheduler requires a sqlite-backed Repo".into()))?;
        let occupancy = crate::db::sqlite::checkout_occupancy(
            &mut *pool.acquire().await?,
            track_id.as_str(),
            "",
        )
        .await?;
        for task in compute_ready(&tasks, occupancy) {
            self.dispatch_task(task, &track).await;
        }
        Ok(())
    }

    /// Claim one ready task and drive its worker spawn. Every failure mode is contained
    /// here; the pass continues with its remaining ready tasks.
    async fn dispatch_task(self: &Arc<Self>, task: Task, track: &Track) -> bool {
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
        // The spawn is driven off the row the claim tx re-read AFTER winning: the semaphore
        // wait leaves an unbounded window in which a still-pending row can be revised, so the
        // pre-claim snapshot must never feed the payload.
        let pre_claim_task_id = task.id.clone();
        let frozen = match self.claim_task(task, track).await {
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
        if let Err(e) = self.drive_spawn(&frozen, track).await {
            tracing::warn!(
                task_id = %pre_claim_task_id,
                error = %e,
                "scheduler: worker spawn drive failed; sweep will reconcile"
            );
        }
        true
    }

    /// The claim tx, one eventized write: in-tx open re-check, single-winner
    /// `pending → dispatched` UPDATE and `Event::TaskDispatched`. Returns the post-claim re-read (the frozen row); `Ok(None)` = race lost,
    /// no event persisted.
    async fn claim_task(&self, task: Task, track: &Track) -> Result<Option<Task>> {
        let monitor = TaskContextMonitor::new_with_metrics(
            Arc::clone(&self.repo),
            self.events.clone(),
            self.write.clone(),
            Arc::clone(&self.context_metrics),
        );
        let closure = match monitor
            .resolve_task_closure(&task.track_id, &task.key)
            .await
        {
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
        let scope = EventScope::Track {
            track: track.id.clone(),
            area: track.area_id.clone(),
        };
        let task_id = task.id.clone();
        let track_id = track.id.clone();
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
                        // Open gate, re-checked IN the claim tx: the pre-claim read can go stale across
                        // the semaphore wait. Loss is silent (race-lost, no event).
                        let current = track_find_tx(tx, track_id.as_str())
                            .await?
                            .ok_or_else(race_lost_err)?;
                        if !current.is_open() {
                            return Err(race_lost_err());
                        }
                        // Claim fence: missing track/report, a changed root, or any changed report doc_rev is
                        // a silent race loss; runs before the pending -> dispatched flip.
                        for (frozen_track, frozen_rev) in &claim_doc_revs {
                            let current: Option<Option<i64>> = match sqlx::query_as::<_, (Option<i64>,)>(
                                "SELECT json_extract(c.payload, '$.docRev') FROM cards c \
                                 JOIN tracks w ON w.id = c.track_id \
                                 WHERE c.track_id = ?1 AND c.kind = 'track-report' LIMIT 1",
                            )
                            .bind(frozen_track)
                            .fetch_optional(&mut **tx)
                            .await
                            {
                                Ok(row) => row.map(|(value,)| value),
                                Err(error) => {
                                    tracing::warn!(
                                        track_id = frozen_track,
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
                            let snapshot: Option<(String, Option<Vec<u8>>)> = sqlx::query_as(
                                "SELECT payload,body_crdt FROM cards WHERE track_id=?1 AND kind='track-report' LIMIT 1",
                            ).bind(root.track_id.as_str()).fetch_optional(&mut **tx).await?;
                            let current_root = snapshot.and_then(|(payload, crdt)| {
                                let payload = serde_json::from_str::<Value>(&payload).ok()?;
                                let blocks = crate::task_context::context_snapshot_values(
                                    root.track_id.as_str(), &payload, crdt.as_deref(),
                                ).ok()?;
                                blocks.into_iter().find_map(|value| {
                                    let block: calm_types::track_report::ReportBlock = serde_json::from_value(value).ok()?;
                                    (block.id == root.block_id).then(|| context_ref(root.track_id.as_str(), &block, true))
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
                        // Revalidate admission against the CURRENT plan and checkout in the same tx: a
                        // dependency added mid-window, an attempt that took the checkout since the pass
                        // read it, or a writer now waiting ahead of a reader aborts the claim. Which
                        // admitted task claims first is deliberately NOT revalidated.
                        let siblings = tasks_by_track_tx(tx, track_id.as_str()).await?;
                        let occupancy =
                            crate::db::sqlite::checkout_occupancy(tx, track_id.as_str(), &task_id)
                                .await?;
                        if !crate::db::sqlite::checkout_admission(&siblings, occupancy)
                            .iter()
                            .any(|(sibling, wait)| sibling.id == task_id && wait.is_none())
                        {
                            return Err(race_lost_err());
                        }
                        let now = now_ms();
                        let rows =
                            task_claim_pending_tx(tx, &task_id, now, &claim_refs, claim_truncated)
                                .await?;
                        if rows == 0 {
                            return Err(race_lost_err());
                        }
                        // Post-claim re-read = the frozen row. Gone row = concurrent track delete; treat as lost.
                        let frozen = task_get_tx(tx, &task_id).await?.ok_or_else(race_lost_err)?;
                        let events = vec![
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
                                    track_id: track_id.clone(),
                                    task_key: task_key.clone(),
                                    idempotency_key: task_id.clone(),
                                    task_id: task_id.clone(),
                                    refs: claim_refs.clone(),
                                    doc_revs: claim_doc_revs.clone(),
                                    truncated: claim_truncated,
                                },
                            ),
                        ];
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

    /// Build the deterministic payload, submit the worker operation (`idempotency_key =
    /// task.id`), `wait()` it to a terminal phase, then reconcile the row with guarded
    /// writes. Shared between the live dispatch path and the sweep's `dispatched` arm.
    async fn drive_spawn(&self, task: &Task, track: &Track) -> Result<()> {
        if task.spawn == calm_types::task_recovery::TASK_CHILD_TRACK_ROUTE {
            return self.drive_child_track(task, track).await;
        }
        let Some(runtime) = self.operation_runtime.upgrade() else {
            tracing::debug!(
                task_id = %task.id,
                "scheduler: operation runtime dropped; skipping spawn drive"
            );
            return Ok(());
        };
        if task.start == TaskStart::Upstream {
            // #2058 D5: a catch-up starts at the upstream the kernel fetches now; its prepare
            // refuses anything else. Outside every transaction (#1777); a re-drive fetches again.
            refresh_catch_up_upstream(track).await;
        }
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
            // The idempotency payload-hash conflict is PERMANENT: our resubmits always hash-match,
            // so a mismatch is a foreign operation and would retry every sweep while holding the task.
            Err(e) if crate::operation::is_idempotency_payload_conflict(&e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "scheduler: task idempotency key owned by a foreign operation (permanent); failing task"
                );
                return self.fail_spawn(task, track, &e.to_string()).await;
            }
            // Everything else stays TRANSIENT/unknown (policy-free: no
            // retry counting) — log-and-leave for the next trigger/sweep.
            Err(e) => return Err(e),
        };
        let result = runtime.wait(&op_id).await?;
        let outcome = match result.outcome {
            OperationOutcome::Failed {
                last_error,
                from_phase,
                last_error_class,
            } if task.start == TaskStart::Upstream => {
                // #2058 D6 2a: a catch-up whose prepare committed has moved the checkout; its
                // output names where to.
                let note = runtime
                    .find_by_kind_and_idempotency(op_kind, &task.id)
                    .await?
                    .and_then(|op| op.tx_output)
                    .and_then(|output| {
                        crate::operation::workspace_lease::worker::catch_up_spawn_failure_note(
                            &output,
                        )
                    });
                OperationOutcome::Failed {
                    last_error: match note {
                        Some(note) => format!("{last_error}. {note}"),
                        None => last_error,
                    },
                    from_phase,
                    last_error_class,
                }
            }
            other => other,
        };
        self.reconcile_spawn_result(task, track, outcome).await
    }

    async fn drive_child_track(&self, task: &Task, track: &Track) -> Result<()> {
        let Some(runtime) = self.operation_runtime.upgrade() else {
            tracing::debug!(task_id = %task.id, "scheduler: operation runtime dropped; skipping child-track drive");
            return Ok(());
        };
        let payload = build_child_track_payload(task)?;
        let op_id = runtime
            .submit(
                CHILD_TRACK_KIND,
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
                let code = if last_error.contains("sub-track-depth-exceeded") {
                    "sub-track-depth-exceeded"
                } else if last_error.contains("sub-track-tree-cycle") {
                    "sub-track-tree-cycle"
                } else {
                    "child-track-create-failed"
                };
                return self
                    .fail_child_track_task(task, track, code, &last_error)
                    .await;
            }
            OperationOutcome::Stuck { reason, .. } => {
                return self
                    .fail_child_track_task(task, track, "child-track-create-stuck", &reason)
                    .await;
            }
        };
        let child_id = result
            .get("child_track_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CalmError::Internal("child-track result missing child_track_id".into())
            })?;
        let planner_card_id = result
            .get("planner_card_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CalmError::Internal("child-track result missing planner_card_id".into())
            })?;
        let report_card_id = result
            .get("report_card_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CalmError::Internal("child-track result missing report_card_id".into())
            })?;
        let cwd = result
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("child-track result missing cwd".into()))?;
        let seed = result
            .get("seed")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("child-track result missing seed".into()))?;

        let bootstrap = PlannerHarnessStartOperationPayload {
            actor: ActorId::KernelDispatcher,
            track_id: child_id.to_string(),
            planner_card_id: crate::ids::CardId::from(planner_card_id.to_string()),
            report_card_id: Some(report_card_id.to_string()),
            sort: None,
            cwd: cwd.to_string(),
            goal: Some(seed.to_string()),
            reset_harness_items: false,
            force_new_thread: false,
            profile: Default::default(),
            create_card: None,
            first_message: None,
            create_request_sha256: None,
            // Not a conversation create; nothing to brief.
            opening_briefing: None,
        };
        // Under the child card's start fence (#2275), like every other start: a reset or send of
        // the just-minted child waits for this start instead of interleaving with it. Held from
        // the submit through the wait, and nothing else: the scheduler holds its per-track pass
        // lock and a spawn permit here, which no fence holder ever waits on, and neither the drive
        // mutex nor `track_delete_locks` (see `state.rs`'s lock order).
        let route_repo: Arc<dyn RouteRepo> = self.repo.clone();
        let fence = CardStartFence::lock(
            &self.planner_recovery_locks,
            &route_repo,
            &runtime,
            &bootstrap.planner_card_id,
        )
        .await;
        let key = OperationKey {
            operation_key: new_id(),
            // The key carries a digest of the cwd: the runtime refuses "same key, different
            // payload hash" permanently, so a re-pointed child would otherwise fail forever.
            idempotency_key: Some(format!(
                "child-track:{child_id}:bootstrap:{}",
                crate::workspace_materialize::workspace_key_digest(cwd)
            )),
            payload_hash: stable_payload_hash(&serde_json::to_value(&bootstrap)?)?,
        };
        let outcome = fence.start(&bootstrap, key).await?.outcome;
        drop(fence);
        // Bootstrap is strictly before the dispatched→running flip. A crash
        // can therefore only leave a dispatched row, which resume re-drives.
        match outcome {
            OperationOutcome::Succeeded { .. } | OperationOutcome::SucceededViaCollision { .. } => {
                self.mark_sub_track_running(&task.id).await
            }
            OperationOutcome::Failed { last_error, .. } => {
                self.fail_child_track_task(task, track, "child-track-bootstrap-failed", &last_error)
                    .await
            }
            OperationOutcome::Stuck { reason, .. } => {
                self.fail_child_track_task(task, track, "child-track-bootstrap-stuck", &reason)
                    .await
            }
        }
    }

    async fn mark_sub_track_running(&self, task_id: &str) -> Result<()> {
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("scheduler requires a sqlite-backed Repo".into()))?;
        let mut tx = begin_immediate_tx(&pool).await?;
        task_mark_sub_track_running_tx(&mut tx, task_id, now_ms()).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn fail_child_track_task(
        &self,
        task: &Task,
        track: &Track,
        code: &str,
        detail: &str,
    ) -> Result<()> {
        let task_id = task.id.clone();
        let track_id = track.id.clone();
        let scope = EventScope::Track {
            track: track.id.clone(),
            area: track.area_id.clone(),
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
                    // The child-track operation may fail after prepare_tx has committed the child, so
                    // derive cleanup ownership from durable task state in the same transaction as the flip.
                    let child_id: Option<String> = sqlx::query_scalar(
                        "SELECT child_track_id FROM tasks WHERE id=?1 AND track_id=?2",
                    )
                    .bind(&task_id)
                    .bind(track_id.as_str())
                    .fetch_optional(&mut **tx)
                    .await?
                    .flatten();
                    let rows = task_fail_from_worker_tx(
                        tx,
                        &task_id,
                        track_id.as_str(),
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
                        let child_track_id = TrackId::from(child_id);
                        let current = crate::db::sqlite::track_get_tx(tx, &child_track_id).await?;
                        if current.is_open() {
                            let updated = crate::db::sqlite::track_update_tx(
                                tx,
                                child_track_id.as_str(),
                                crate::model::TrackPatch {
                                    closed: Some(true),
                                    ..Default::default()
                                },
                            )
                            .await?;
                            let child_scope = EventScope::Track {
                                track: updated.id.clone(),
                                area: updated.area_id.clone(),
                            };
                            events.push((
                                child_scope,
                                Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
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
                            details: None,
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
        track: &Track,
        outcome: OperationOutcome,
    ) -> Result<()> {
        match outcome {
            OperationOutcome::Succeeded { result }
            | OperationOutcome::SucceededViaCollision { result, .. } => {
                // Guarded `dispatched → running` + `worker_card_id` stamp; a missing id leaves the
                // stamp to the report tx's COALESCE.
                let worker_card_id = result.get("id").and_then(Value::as_str).map(str::to_string);
                self.mark_running(&task.id, worker_card_id.as_deref())
                    .await?;
                // A terminal task resumed by the boot sweep may already carry a recorded exit;
                // reconcile now instead of waiting for the next sweep. A just-spawned terminal no-ops.
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
                self.fail_spawn(task, track, &last_error).await?;
            }
            OperationOutcome::Stuck { reason, .. } => {
                self.fail_spawn(task, track, &reason).await?;
            }
        }
        Ok(())
    }

    /// Guarded running stamp, no event: 0 rows = a fast worker report already advanced
    /// the row — by design, not an error.
    async fn mark_running(&self, task_id: &str, worker_card_id: Option<&str>) -> Result<()> {
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("scheduler requires a sqlite-backed Repo".into()))?;
        let mut tx = begin_immediate_tx(&pool).await?;
        let rows = mark_acknowledged_running_tx(
            &mut tx,
            task_id,
            worker_card_id,
            self.worker_liveness.cap_ms(),
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

    /// Spawn failure/stuck: guarded `dispatched/running → failed('spawn-failed: <reason>')` plus
    /// kernel `task.failed` in one tx. 0-row
    /// flip → the row already moved on; no event. The `spawn-failed` CLASSIFIER stays the prefix.
    async fn fail_spawn(&self, task: &Task, track: &Track, reason: &str) -> Result<()> {
        let task = task.clone();
        let track = track.clone();
        let reason = reason.to_string();
        let result = write_with_actor_events_typed::<(), _>(
            self.repo.as_ref(),
            None,
            &self.events,
            &self.write,
            move |tx| {
                Box::pin(async move {
                    let events =
                        fail_worker_task_tx(tx, &task, &track, "spawn-failed", &reason).await?;
                    Ok(((), events))
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

    /// Sweep body shared between boot, the periodic reconcile tick, and `Lagged`.
    /// Boot-gated: both backstop callers are spawned before boot recovery, so until
    /// `sweep_boot` completes this is a no-op.
    pub async fn sweep_all(self: &Arc<Self>) {
        if !self.boot_sweep_done.load(Ordering::SeqCst) {
            tracing::debug!(
                "scheduler: backstop sweep skipped — boot recovery/sweep has not completed yet"
            );
            return;
        }
        let pending_tracks = self.sweep_reconcile().await;
        for track_id in pending_tracks {
            self.schedule_track(TrackId::from(track_id)).await;
        }
    }

    /// Boot-time sweep: the reconcile arms run synchronously (after operation recovery) but
    /// pending-arm dispatching goes through async `poke` so boot never blocks the HTTP
    /// server. Completing it opens the boot gate.
    pub async fn sweep_boot(self: &Arc<Self>) {
        let pending_tracks = self.sweep_reconcile().await;
        for track_id in pending_tracks {
            self.poke(TrackId::from(track_id));
        }
        self.boot_sweep_done.store(true, Ordering::SeqCst);
    }

    /// Whether the boot gate is open. Exposed for test assertions.
    pub fn boot_sweep_completed(&self) -> bool {
        self.boot_sweep_done.load(Ordering::SeqCst)
    }

    /// TEST seam: open the boot gate without running a boot sweep.
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

    /// Shared sweep body: runs the reconcile arms inline and returns the set of tracks
    /// holding `pending` rows for the caller to dispatch.
    async fn sweep_reconcile(self: &Arc<Self>) -> BTreeSet<String> {
        // The parked sweep recovers durable verdicts from dead gates and kill-fails
        // past-deadline work; every arm is fenced single-winner, so racing the live observer is safe.
        if let Some(runtime) = self.operation_runtime.upgrade()
            && let Err(e) = runtime.sweep_parked().await
        {
            tracing::warn!(error = %e, "scheduler sweep: sweep_parked failed; next tick retries");
        }
        self.sweep_timeout_worker_cleanups().await;
        let mut pending_tracks: BTreeSet<String> = BTreeSet::new();
        // Unsettled git deliveries are the authoritative discovery, whatever the task status (F6.4).
        pending_tracks.extend(self.unsettled_git_delivery_tracks().await);
        let tasks = match self.repo.tasks_nonterminal().await {
            Ok(tasks) => tasks,
            Err(e) => {
                tracing::warn!(error = %e, "scheduler sweep: task scan failed; skipping");
                return pending_tracks;
            }
        };
        for task in tasks {
            match task.status {
                TaskStatus::Pending => {
                    pending_tracks.insert(task.track_id.clone());
                }
                TaskStatus::Dispatched => {
                    self.resume_dispatched(task).await;
                }
                // Must precede both terminal reconciliation and kind-based
                // timeout arms. Sub-track parents have their own child-state
                // reconciler and never receive a worker deadline.
                TaskStatus::Running if task.spawn == "sub-wave" => {}
                TaskStatus::Running if task.kind == TaskKind::Terminal => {
                    self.reconcile_running_terminal(task).await;
                }
                TaskStatus::Running if task_has_running_liveness_deadline(&task) => {
                    let card_id = self.worker_card_id_for_task(&task).await;
                    let facts = match self
                        .running_liveness_facts(&task.id, card_id.as_deref())
                        .await
                    {
                        Ok(Some(facts)) => facts,
                        // The row left `running` since the scan.
                        Ok(None) => continue,
                        Err(e) => {
                            tracing::warn!(
                                task_id = %task.id,
                                error = %e,
                                "scheduler sweep: running liveness read failed; next sweep retries"
                            );
                            continue;
                        }
                    };
                    match self
                        .worker_liveness
                        .expiry(&facts, now_ms(), self.liveness_floor())
                    {
                        Some(expiry) => {
                            self.fail_running_worker(
                                task,
                                RunningWorkerFailure::LivenessTimeout(expiry),
                            )
                            .await;
                        }
                        None => self.spawn_worker_idle_check(&task),
                    }
                }
                TaskStatus::Running => {}
                // Drive the current gate attempt. Spawned because a gate watch can outlive the sweep
                // by hours; dead-parked enforcement is `sweep_parked`'s job above.
                TaskStatus::Verifying => {
                    let this = Arc::clone(self);
                    tokio::spawn(async move {
                        this.drive_gate(task).await;
                    });
                }
                TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled => {}
            }
        }
        self.reconcile_all_child_track_tasks().await;
        pending_tracks
    }

    /// The running worker's liveness facts, first stamping a start or deadline the row lacks
    /// (rows already running before either column existed). `None`: the row is not `running`.
    async fn running_liveness_facts(
        &self,
        task_id: &str,
        worker_card_id: Option<&str>,
    ) -> Result<Option<RunningLivenessFacts>> {
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("scheduler requires a sqlite-backed Repo".into()))?;
        let mut tx = begin_immediate_tx(&pool).await?;
        let now = now_ms();
        let deadline = now.saturating_add(self.worker_liveness.cap_ms());
        task_stamp_missing_running_liveness_tx(&mut tx, task_id, now, deadline).await?;
        let facts = task_running_liveness_tx(&mut tx, task_id, worker_card_id).await?;
        tx.commit().await?;
        Ok(facts)
    }

    /// When this scheduler booted, or the fixture's stand-in for it.
    pub(super) fn liveness_floor(&self) -> i64 {
        self.liveness_floor_ms.load(Ordering::SeqCst)
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
            // #1830 S2 D7: the worker is stopped, so its attempt is committed now, as its
            // terminal `tasks.status` says (`failed` for a timeout, `canceled` for a cancel).
            if let Err(e) = release_workspace_lease_for_card_repo(
                self.repo.as_ref(),
                &self.events,
                &cleanup.card_id,
                ReleaseDelivery::CommitAsTaskEnded,
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
        let (operation_kind, _) = build_worker_payload(task).ok()?;
        self.operation_runtime
            .upgrade()?
            .find_by_kind_and_idempotency(operation_kind, &task.id)
            .await
            .ok()
            .flatten()
            .filter(|op| op.target_type == "card")
            .and_then(|op| op.target_id)
    }

    /// Sweep `dispatched` arm: the claim landed but the spawn outcome was never reconciled.
    /// `drive_spawn` covers every sub-case via submit-dedupe + `wait()` + guarded writes.
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
        let track = match self.repo.track_get(&task.track_id).await {
            Ok(Some(track)) => track,
            Ok(None) => {
                tracing::warn!(
                    task_id = %task.id,
                    "scheduler sweep: dispatched task's track row is gone; leaving row"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(task_id = %task.id, error = %e, "scheduler sweep: track_get failed");
                return;
            }
        };
        let _permit = match Arc::clone(&self.semaphore).acquire_owned().await {
            Ok(p) => p,
            Err(_) => return,
        };
        if let Err(e) = self.drive_spawn(&task, &track).await {
            tracing::warn!(
                task_id = %task.id,
                error = %e,
                "scheduler sweep: dispatched-arm drive failed; next sweep retries"
            );
        }
    }

    /// Sweep `running`-terminal arm: a recorded exit gets the SAME guarded completion tx as
    /// the live exit hook; first writer wins via the status guard.
    async fn reconcile_running_terminal(&self, task: Task) {
        let worker_card_id = match &task.worker_card_id {
            Some(id) => Some(id.clone()),
            // Crash between op success and the running stamp can leave the card unstamped;
            // recover it from the operation row (idempotency-key convention).
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
            &task.track_id,
            &card_id,
            terminal.exit_code,
            terminal.signal_killed,
            &terminal.pty_output,
            terminal.pty_output_truncated,
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

    /// Drive one `verifying` task's gate. Single-flight per task under `"gate:{task.id}"`,
    /// disjoint from the worker-spawn keyspace. Deliberately does NOT hold the dispatch
    /// semaphore: the `wait()` can span a multi-hour gate.
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

    /// If the current attempt's op exists, `wait()` it and copy the outcome iff the row is
    /// still `verifying` at that attempt; otherwise submit `#g{gate_attempt + 1}`. Racing
    /// submitters compute the same key and dedupe on the operations unique index.
    async fn drive_gate_inner(
        self: &Arc<Self>,
        runtime: &Arc<OperationRuntime>,
        task: &Task,
    ) -> Result<()> {
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
        // Admission (#1727 S4 D3, oracle 8) decides only whether a NEW op is submitted: a
        // candidate-bound attempt's gate waits for its delivery to settle as a candidate;
        // `Failed` / still pending → no `#gN`. An existing `#gN` (above) is always
        // waited and reconciled.
        if !self.admit_gate(runtime, task).await? {
            return Ok(());
        }
        let attempt = task.gate_attempt + 1;
        let payload = serde_json::to_value(TaskVerifyOperationPayload {
            actor: ActorId::KernelDispatcher,
            track_id: task.track_id.clone(),
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

    /// "Row `verifying`, op terminal → copy the outcome to the row"; needed because op-only
    /// terminal writes exist. Same one-tx body as the live observer, so first writer wins
    /// on the `status='verifying' AND gate_attempt=N` guard.
    async fn reconcile_gate_outcome(
        &self,
        task: &Task,
        attempt: i64,
        log_path: &str,
        outcome: OperationOutcome,
    ) -> Result<()> {
        // Only a failed/stuck op is eligible for the pre-bump fallback below: an op that
        // reached a verdict necessarily ran `prepare_tx`'s bump first.
        let op_terminal_failed = matches!(
            outcome,
            OperationOutcome::Failed { .. } | OperationOutcome::Stuck { .. }
        );
        let pool = self
            .repo
            .sqlite_pool()
            .ok_or_else(|| CalmError::Internal("scheduler requires a sqlite-backed Repo".into()))?;
        let Some(track) = self.repo.track_get(&task.track_id).await? else {
            tracing::debug!(task_id = %task.id, "scheduler: gate task's track row is gone");
            return Ok(());
        };
        let rctx = GateResultCtx {
            task_id: task.id.clone(),
            track_id: track.id.clone(),
            area_id: track.area_id.clone(),
        };
        let mut tx = begin_immediate_tx(&pool).await?;
        // P9 / P9b / P10: the verdict with its target (frozen, or derived from the rows).
        let verdict = crate::operation::task_verify_adapter::target::reconcile_result_tx(
            &mut tx, task, attempt, log_path, outcome,
        )
        .await?;
        let mut envelopes = apply_gate_result_in_tx(&mut tx, &rctx, &verdict).await?;
        if envelopes.is_empty() && op_terminal_failed && verdict.verdict.attempt >= 1 {
            // Pre-bump failure arm: a `prepare_tx` error BEFORE the bump terminal-fails op `#gN`
            // while the row stays `verifying@N-1`, and the eq-attempt guard would miss forever.
            // Flip at the pre-bump attempt; a row that DID reach attempt N makes this guard miss.
            envelopes = crate::operation::task_verify_adapter::apply_gate_result_with_guard_in_tx(
                &mut tx,
                &rctx,
                &verdict,
                verdict.verdict.attempt - 1,
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

/// Terminal-exit completion bundle (live path), threaded into the terminal renderer
/// registry so the attach-reader exit branch can run the shared guarded completion tx.
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

    /// Live exit path: resolve terminal → card → payload `idempotency_key` to a plan-task row
    /// and run the shared guarded completion tx. The payload walk only FINDS the candidate;
    /// ownership is proven inside the tx (card payloads are patchable, so they are not proof).
    pub async fn on_terminal_exit(
        &self,
        terminal_id: &str,
        exit_code: Option<i32>,
        signal_killed: bool,
        pty_output: &str,
        pty_output_truncated: bool,
    ) {
        let terminal = match self.repo.terminal_get(terminal_id).await {
            Ok(Some(term)) => term,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(terminal_id, error = %e, "terminal task hook: terminal_get failed");
                return;
            }
        };
        let card_id = terminal.card_id.clone();
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
        // Only TERMINAL-kind tasks are mechanically reconcilable from a PTY exit code; a codex
        // PTY exiting 0 says nothing about the task.
        if task.kind != TaskKind::Terminal {
            return;
        }
        if let Err(e) = complete_terminal_task(
            self.repo.as_ref(),
            &self.events,
            &self.write,
            &task.id,
            &task.track_id,
            card_id.as_str(),
            exit_code,
            signal_killed,
            pty_output,
            pty_output_truncated,
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

/// The ONE guarded terminal-completion function — the live exit hook and the sweep arm
/// run exactly this tx; first writer wins via the `status IN ('dispatched','running')`
/// guard. Exit 0 → `task.completed`; non-zero / signal / synthetic `-1` → `task.failed`.
#[allow(clippy::too_many_arguments)]
pub async fn complete_terminal_task(
    repo: &dyn Repo,
    events: &EventBus,
    write: &WriteContext,
    task_id: &str,
    track_id: &str,
    worker_card_id: &str,
    exit_code: Option<i32>,
    signal_killed: bool,
    pty_output: &str,
    pty_output_truncated: bool,
) -> Result<()> {
    let Some(track) = repo.track_get(track_id).await? else {
        return Ok(());
    };
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let success = !signal_killed && exit_code == Some(0);
    let task_id = task_id.to_string();
    let track_id_str = track_id.to_string();
    let worker_card_id = worker_card_id.to_string();
    let pty_output = pty_output.to_string();
    let result = write_with_actor_events_typed::<(), _>(repo, None, events, write, move |tx| {
        Box::pin(async move {
            let now = now_ms();
            // The payload `idempotency_key` is mutable via `PATCH /api/cards/{id}`, so it is NOT
            // proof of ownership: only the card the worker-spawn op actually created may flip an
            // UNSTAMPED row; a forged-payload card fails both sides → 0 rows → no event.
            let owns_key =
                crate::db::sqlite::worker_op_targets_card_tx(tx, &task_id, &worker_card_id).await?;
            let reporter = TaskReporter::Card {
                card_id: worker_card_id.as_str(),
                owns_key,
            };
            // A gated terminal task's clean exit is still a self-report: the row goes to `verifying`.
            let (rows, event) = if success {
                let flip =
                    task_report_success_from_worker_tx(tx, &task_id, &track_id_str, reporter, now)
                        .await?;
                (
                    if flip == SuccessReportFlip::None {
                        0
                    } else {
                        1
                    },
                    Event::TaskCompleted {
                        idempotency_key: task_id.clone(),
                        result: json!({
                            "exit_code": 0,
                            "source": "terminal-exit",
                            "pty_output": pty_output,
                            "pty_output_truncated": pty_output_truncated,
                        }),
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
                    // The same interpreted reason the event carries lands on the row.
                    task_fail_from_worker_tx(
                        tx,
                        &task_id,
                        &track_id_str,
                        reporter,
                        &status_detail_with_reason("worker-reported", &reason),
                        now,
                    )
                    .await?,
                    Event::TaskFailed {
                        idempotency_key: task_id.clone(),
                        reason,
                        details: Some(json!({
                            "exit_code": exit_code,
                            "signal_killed": signal_killed,
                            "source": "terminal-exit",
                            "pty_output": pty_output,
                            "pty_output_truncated": pty_output_truncated,
                        })),
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
            Ok(((), vec![(ActorId::KernelDispatcher, scope, event)]))
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

/// Canonical acknowledgement stamp shared by terminal startup and parked worker startup.
/// A fast terminal report remains authoritative: the existing CAS returns zero.
pub(crate) async fn mark_acknowledged_running_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    task_id: &str,
    worker_card_id: Option<&str>,
    timeout_ms: i64,
) -> Result<u64> {
    let now = now_ms();
    task_mark_running_tx(
        tx,
        task_id,
        worker_card_id,
        now,
        now.saturating_add(timeout_ms),
    )
    .await
}
