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
//! never times a worker out, and never garbage-collects. Retry is the
//! spec inserting a new task. The only judgment it holds is the
//! ready-set predicate.
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
//! handling is a no-op.
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

use dashmap::DashMap;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::db::sqlite::{
    begin_immediate_tx, task_claim_pending_tx, task_complete_from_worker_tx,
    task_fail_from_worker_tx, task_get_tx, task_mark_running_tx, wave_lifecycle_tx,
};
use crate::db::{Repo, write_with_actor_events_typed};
use crate::error::{CalmError, Result};
use crate::event::{Event, EventBus, EventScope};
use crate::ids::{ActorId, WaveId};
use crate::model::{Task, TaskKind, TaskStatus, Wave, WaveLifecycle, new_id, now_ms};
use crate::operation::codex_adapter::CodexWorkerOperationPayload;
use crate::operation::terminal_adapter::{
    TerminalWorkerOperationPayload, normalize_terminal_worker_cwd,
};
use crate::operation::{OperationKey, OperationOutcome, OperationRuntime};
use crate::routes::terminal_cards::stable_payload_hash;
use crate::state::WriteContext;
use crate::wave_lifecycle::auto_transition_if_current_in_tx;

/// Kernel default per-wave task budget when `waves.task_budget` is NULL
/// and `NEIGE_WAVE_TASK_BUDGET` is unset/invalid. **1 is deliberate**
/// (§5.3): workers and gates share one directory tree today (no
/// worktrees, risk R2); >1 is opt-in per wave.
pub const DEFAULT_WAVE_TASK_BUDGET: i64 = 1;

/// Default reconcile-tick period (§5.1 liveness backstop).
pub const DEFAULT_RECONCILE_SECS: u64 = 300;

/// Internal sentinel: a guarded flip affected 0 rows because another
/// writer (claim race, fast worker report, earlier sweep) won. Carried
/// through `CalmError::Conflict` so the eventized-write helper rolls
/// the tx back without persisting events; callers translate it back
/// into a silent no-op.
const RACE_LOST: &str = "scheduler: race lost (guarded write no-op)";

fn race_lost_err() -> CalmError {
    CalmError::Conflict(RACE_LOST.into())
}

fn is_race_lost(e: &CalmError) -> bool {
    matches!(e, CalmError::Conflict(m) if m == RACE_LOST)
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
pub fn compute_ready(tasks: &[Task], budget: i64) -> Vec<Task> {
    let done_keys: BTreeSet<&str> = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .map(|t| t.key.as_str())
        .collect();
    let running_cost = tasks
        .iter()
        .filter(|t| {
            matches!(
                t.status,
                TaskStatus::Dispatched | TaskStatus::Running | TaskStatus::Verifying
            )
        })
        .count() as i64;
    let capacity = (budget - running_cost).max(0) as usize;
    tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Pending)
        .filter(|t| {
            t.depends_on()
                .iter()
                .all(|dep| done_keys.contains(dep.as_str()))
        })
        .take(capacity)
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
                context: serde_json::from_str(&task.context_json).unwrap_or(Value::Null),
                acceptance_criteria: task.acceptance_criteria.clone(),
            })?;
            Ok(("codex-worker", payload))
        }
        TaskKind::Terminal => {
            let payload = serde_json::to_value(TerminalWorkerOperationPayload {
                actor: ActorId::KernelDispatcher,
                wave_id: task.wave_id.clone(),
                idempotency_key: task.id.clone(),
                cmd: task.goal.clone(),
                cwd: Some(normalize_terminal_worker_cwd(task.cwd.clone())),
            })?;
            Ok(("terminal-worker", payload))
        }
    }
}

fn task_kind_str(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Codex => "codex",
        TaskKind::Terminal => "terminal",
    }
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
    /// §5.1 per-wave single-flight: exactly the push-locks pattern.
    wave_locks: DashMap<WaveId, Arc<tokio::sync::Mutex<()>>>,
    /// Dirty flags — a trigger arriving mid-pass marks dirty and the
    /// lock holder loops once more, so no envelope is ever lost to "a
    /// pass was already running".
    wave_dirty: DashMap<WaveId, Arc<AtomicBool>>,
    /// Per-task single-flight for submit/wait drives (live + sweep).
    inflight: Arc<DashMap<String, ()>>,
}

impl Scheduler {
    pub fn new(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        operation_runtime: Weak<OperationRuntime>,
        semaphore: Arc<Semaphore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            repo,
            events,
            write,
            operation_runtime,
            semaphore,
            budget_default: Self::budget_from_env(DEFAULT_WAVE_TASK_BUDGET),
            wave_locks: DashMap::new(),
            wave_dirty: DashMap::new(),
            inflight: Arc::new(DashMap::new()),
        })
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

    /// Resolve the reconcile-tick period from
    /// `NEIGE_SCHEDULER_RECONCILE_SECS` (default 300; non-positive /
    /// garbage → default).
    pub fn reconcile_secs_from_env(default: u64) -> u64 {
        match std::env::var("NEIGE_SCHEDULER_RECONCILE_SECS") {
            Ok(raw) => match raw.trim().parse::<u64>() {
                Ok(n) if n > 0 => n,
                _ => default,
            },
            Err(_) => default,
        }
    }

    /// Configured kernel-default budget. Exposed for test assertions.
    pub fn budget_default(&self) -> i64 {
        self.budget_default
    }

    /// Fire-and-forget trigger: schedule the wave on a fresh task. Used
    /// by the dispatcher's envelope arms.
    pub fn poke(self: &Arc<Self>, wave_id: WaveId) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.schedule_wave(wave_id).await;
        });
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
        if !lifecycle_allows_scheduling(wave.lifecycle) {
            tracing::debug!(
                wave_id = %wave_id,
                lifecycle = ?wave.lifecycle,
                "scheduler: lifecycle holds scheduling; skipping pass"
            );
            return Ok(());
        }
        let budget = self.wave_budget(wave_id).await?;
        let tasks = self.repo.tasks_by_wave(wave_id.as_str()).await?;
        let ready = compute_ready(&tasks, budget);
        for task in ready {
            self.dispatch_task(task, &wave).await;
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
    async fn dispatch_task(self: &Arc<Self>, task: Task, wave: &Wave) {
        let Some(_inflight) = InflightGuard::acquire(&self.inflight, &task.id) else {
            tracing::debug!(task_id = %task.id, "scheduler: task already in flight; skipping");
            return;
        };
        // Global spawn cap — same semaphore the dispatcher holds across
        // its spawn handling.
        let _permit = match Arc::clone(&self.semaphore).acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("scheduler: dispatcher semaphore closed; skipping dispatch");
                return;
            }
        };
        // The spawn is driven off the row the claim tx itself re-read
        // AFTER winning (review F2): the semaphore wait above leaves an
        // unbounded window in which a still-pending row can be revised
        // or re-kinded, so the pre-claim snapshot must never feed the
        // payload. Post-claim the row is frozen — every plan mutation
        // path is `WHERE status = 'pending'`.
        let frozen = match self.claim_task(&task, wave).await {
            Ok(Some(frozen)) => frozen,
            Ok(None) => return, // someone else won the claim
            Err(e) => {
                tracing::warn!(
                    task_id = %task.id,
                    error = %e,
                    "scheduler: claim tx failed; task stays pending for the next trigger"
                );
                return;
            }
        };
        if let Err(e) = self.drive_spawn(&frozen, wave).await {
            tracing::warn!(
                task_id = %task.id,
                error = %e,
                "scheduler: worker spawn drive failed; sweep will reconcile"
            );
        }
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
    /// (review F4), or the wave row was deleted. No event is persisted.
    async fn claim_task(&self, task: &Task, wave: &Wave) -> Result<Option<Task>> {
        let scope = EventScope::Wave {
            wave: wave.id.clone(),
            cove: wave.cove_id.clone(),
        };
        let task_id = task.id.clone();
        let wave_id = wave.id.clone();
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
                        let lifecycle = wave_lifecycle_tx(tx, wave_id.as_str())
                            .await?
                            .ok_or_else(race_lost_err)?;
                        if !lifecycle_allows_scheduling(lifecycle) {
                            return Err(race_lost_err());
                        }
                        let rows = task_claim_pending_tx(tx, &task_id, now_ms()).await?;
                        if rows == 0 {
                            return Err(race_lost_err());
                        }
                        // Post-claim re-read = the frozen row (review F2).
                        // Gone row = concurrent wave delete; treat as lost.
                        let frozen = task_get_tx(tx, &task_id).await?.ok_or_else(race_lost_err)?;
                        let mut events = vec![(
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
                        )];
                        // Same pre-spawn ordering rationale as the legacy
                        // dispatch path: promote before the worker exists so
                        // a fast report's Working → Reviewing promotion can
                        // never race ahead of this one. §5.2 deliberately
                        // schedules Planning waves (review F5) — a wave the
                        // spec never moved past Planning is promoted along
                        // the legal kernel chain Planning → Dispatching →
                        // Working here, so a successful claim always leaves
                        // the wave `Working` and the later Working →
                        // Reviewing auto-transition can fire.
                        for (from, to) in [
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
        let Some(runtime) = self.operation_runtime.upgrade() else {
            tracing::debug!(
                task_id = %task.id,
                "scheduler: operation runtime dropped; skipping spawn drive"
            );
            return Ok(());
        };
        let (op_kind, payload) = build_worker_payload(task)?;
        let payload_hash = stable_payload_hash(&payload)?;
        let op_id = runtime
            .submit(
                op_kind,
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: Some(task.id.clone()),
                    payload_hash,
                },
                payload,
            )
            .await?;
        let result = runtime.wait(&op_id).await?;
        match result.outcome {
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
        let rows = task_mark_running_tx(&mut tx, task_id, worker_card_id, now_ms()).await?;
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
                        None,
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
    /// - `running` + codex kind: left alone (policy-free; risk R4).
    /// - `verifying`: PR-C.
    pub async fn sweep_all(self: &Arc<Self>) {
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
    pub async fn sweep_boot(self: &Arc<Self>) {
        let pending_waves = self.sweep_reconcile().await;
        for wave_id in pending_waves {
            self.poke(WaveId::from(wave_id));
        }
    }

    /// Shared sweep body: runs the reconcile arms inline and returns
    /// the set of waves holding `pending` rows for the caller to
    /// dispatch (blocking in [`Scheduler::sweep_all`], fire-and-forget
    /// in [`Scheduler::sweep_boot`]).
    async fn sweep_reconcile(self: &Arc<Self>) -> BTreeSet<String> {
        let mut pending_waves: BTreeSet<String> = BTreeSet::new();
        let tasks = match self.repo.tasks_nonterminal().await {
            Ok(tasks) => tasks,
            Err(e) => {
                tracing::warn!(error = %e, "scheduler sweep: task scan failed; skipping");
                return pending_waves;
            }
        };
        for task in tasks {
            match task.status {
                TaskStatus::Pending => {
                    pending_waves.insert(task.wave_id.clone());
                }
                TaskStatus::Dispatched => {
                    self.resume_dispatched(task).await;
                }
                TaskStatus::Running if task.kind == TaskKind::Terminal => {
                    self.reconcile_running_terminal(task).await;
                }
                // Policy-free: a running codex worker survives restarts
                // (PTY under proc-supervisor) and reports via the emit
                // tx; the scheduler holds no liveness judgment (R4).
                TaskStatus::Running => {}
                // PR-C territory; the gate sweep arms land with the
                // gate runner.
                TaskStatus::Verifying => {}
                TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled => {}
            }
        }
        pending_waves
    }

    /// Sweep `dispatched` arm (§5.5/§8): the claim landed but the spawn
    /// outcome was never reconciled (crash between claim and op insert,
    /// between op success and the running stamp, or a lost completion).
    /// `drive_spawn` covers every sub-case via submit-dedupe + `wait()`
    /// + guarded reconcile writes.
    async fn resume_dispatched(self: &Arc<Self>, task: Task) {
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
    /// shared guarded completion tx. Terminals with no task row (user
    /// terminals, legacy `calm.task.dispatch` workers, spec terminals)
    /// no-op here.
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
            let (rows, event) = if success {
                (
                    task_complete_from_worker_tx(
                        tx,
                        &task_id,
                        &wave_id_str,
                        Some(worker_card_id.as_str()),
                        now,
                    )
                    .await?,
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
                        Some(worker_card_id.as_str()),
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
            if let Some(auto_events) = auto_transition_if_current_in_tx(
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
mod tests {
    use super::*;

    fn task(key: &str, status: TaskStatus, deps: &[&str], priority: i64) -> Task {
        Task {
            id: format!("w:{key}"),
            wave_id: "w".into(),
            key: key.into(),
            kind: TaskKind::Codex,
            goal: "do".into(),
            context_json: "null".into(),
            acceptance_criteria: None,
            cwd: None,
            depends_on_json: serde_json::to_string(deps).unwrap(),
            priority,
            gate_json: None,
            status,
            status_detail: None,
            worker_card_id: None,
            gate_result_json: None,
            gate_attempt: 0,
            gate_pid: None,
            gate_pid_starttime: None,
            gate_pid_boot_id: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            finished_at_ms: None,
        }
    }

    fn keys(tasks: &[Task]) -> Vec<&str> {
        tasks.iter().map(|t| t.key.as_str()).collect()
    }

    // ---------------------------------------------------- ready set (§5.2)

    #[test]
    fn ready_set_requires_all_deps_done() {
        let tasks = vec![
            task("a", TaskStatus::Done, &[], 0),
            task("b", TaskStatus::Pending, &["a"], 0),
            task("c", TaskStatus::Pending, &["a", "b"], 0),
            task("d", TaskStatus::Pending, &["ghost"], 0),
        ];
        let ready = compute_ready(&tasks, 10);
        assert_eq!(keys(&ready), vec!["b"], "only b has all deps done");
    }

    #[test]
    fn canceled_and_failed_deps_never_satisfy() {
        // §3.1 — deps require `done`; canceled/failed block successors
        // forever (plan-revision authority belongs to the spec).
        let tasks = vec![
            task("a", TaskStatus::Canceled, &[], 0),
            task("b", TaskStatus::Failed, &[], 0),
            task("c", TaskStatus::Pending, &["a"], 0),
            task("d", TaskStatus::Pending, &["b"], 0),
        ];
        assert!(compute_ready(&tasks, 10).is_empty());
    }

    #[test]
    fn budget_counts_dispatched_running_and_verifying() {
        // `verifying` occupies budget deliberately (§5.2) — the SQL/
        // predicate is future-proofed even though no task reaches
        // verifying before PR-C.
        let tasks = vec![
            task("a", TaskStatus::Dispatched, &[], 0),
            task("b", TaskStatus::Running, &[], 0),
            task("c", TaskStatus::Verifying, &[], 0),
            task("d", TaskStatus::Pending, &[], 0),
            task("e", TaskStatus::Pending, &[], 0),
        ];
        assert!(
            compute_ready(&tasks, 3).is_empty(),
            "3 in flight fill budget 3"
        );
        let ready = compute_ready(&tasks, 4);
        assert_eq!(keys(&ready), vec!["d"], "one free slot under budget 4");
        let ready = compute_ready(&tasks, 5);
        assert_eq!(keys(&ready), vec!["d", "e"]);
    }

    #[test]
    fn ready_set_preserves_scheduler_order_and_caps_at_budget() {
        // Input order is the repo's `(priority DESC, created_at ASC,
        // key ASC)`; compute_ready must not reorder (policy-free).
        let mut high = task("zz-high", TaskStatus::Pending, &[], 9);
        high.created_at_ms = 5;
        let tasks = vec![
            high,
            task("aa-low", TaskStatus::Pending, &[], 0),
            task("bb-low", TaskStatus::Pending, &[], 0),
        ];
        let ready = compute_ready(&tasks, 2);
        assert_eq!(keys(&ready), vec!["zz-high", "aa-low"]);
    }

    #[test]
    fn zero_or_negative_capacity_dispatches_nothing() {
        let tasks = vec![
            task("a", TaskStatus::Running, &[], 0),
            task("b", TaskStatus::Pending, &[], 0),
        ];
        assert!(compute_ready(&tasks, 0).is_empty());
        assert!(
            compute_ready(&tasks, 1).is_empty(),
            "running fills budget 1"
        );
    }

    // ---------------------------------------------- lifecycle gating (§5.2)

    #[test]
    fn lifecycle_gating_matches_design_table() {
        for allowed in [
            WaveLifecycle::Planning,
            WaveLifecycle::Dispatching,
            WaveLifecycle::Working,
            WaveLifecycle::Reviewing,
        ] {
            assert!(lifecycle_allows_scheduling(allowed), "{allowed:?}");
        }
        for held in [
            WaveLifecycle::Draft,
            WaveLifecycle::Blocked,
            WaveLifecycle::Done,
            WaveLifecycle::Canceled,
            WaveLifecycle::Failed,
        ] {
            assert!(!lifecycle_allows_scheduling(held), "{held:?}");
        }
    }

    // ------------------------------------------------------- env knobs

    #[test]
    fn budget_from_env_fallback_paths() {
        let saved = std::env::var("NEIGE_WAVE_TASK_BUDGET").ok();
        fn set(v: &str) {
            // SAFETY: single-threaded test; no concurrent env reader.
            unsafe { std::env::set_var("NEIGE_WAVE_TASK_BUDGET", v) };
        }
        fn remove() {
            // SAFETY: see `set`.
            unsafe { std::env::remove_var("NEIGE_WAVE_TASK_BUDGET") };
        }

        remove();
        assert_eq!(Scheduler::budget_from_env(1), 1, "unset → default 1");
        set("");
        assert_eq!(Scheduler::budget_from_env(1), 1, "empty → default");
        set("nope");
        assert_eq!(Scheduler::budget_from_env(1), 1, "garbage → default");
        set("0");
        assert_eq!(Scheduler::budget_from_env(1), 1, "zero → default");
        set("-2");
        assert_eq!(Scheduler::budget_from_env(1), 1, "negative → default");
        set("3");
        assert_eq!(Scheduler::budget_from_env(1), 3, "valid → override");

        match saved {
            Some(v) => set(&v),
            None => remove(),
        }
    }

    #[test]
    fn reconcile_secs_from_env_fallback_paths() {
        let saved = std::env::var("NEIGE_SCHEDULER_RECONCILE_SECS").ok();
        fn set(v: &str) {
            // SAFETY: single-threaded test; no concurrent env reader.
            unsafe { std::env::set_var("NEIGE_SCHEDULER_RECONCILE_SECS", v) };
        }
        fn remove() {
            // SAFETY: see `set`.
            unsafe { std::env::remove_var("NEIGE_SCHEDULER_RECONCILE_SECS") };
        }

        remove();
        assert_eq!(Scheduler::reconcile_secs_from_env(300), 300);
        set("0");
        assert_eq!(Scheduler::reconcile_secs_from_env(300), 300);
        set("17");
        assert_eq!(Scheduler::reconcile_secs_from_env(300), 17);

        match saved {
            Some(v) => set(&v),
            None => remove(),
        }
    }

    // ------------------------------------------------- payload determinism

    #[test]
    fn worker_payload_is_pure_function_of_the_row() {
        let codex = task("a", TaskStatus::Pending, &[], 0);
        let (kind1, p1) = build_worker_payload(&codex).unwrap();
        let (kind2, p2) = build_worker_payload(&codex).unwrap();
        assert_eq!(kind1, "codex-worker");
        assert_eq!(kind1, kind2);
        assert_eq!(p1, p2, "same row → byte-identical payload");
        assert_eq!(
            stable_payload_hash(&p1).unwrap(),
            stable_payload_hash(&p2).unwrap(),
            "same row → same idempotency payload hash (post-crash resubmit matches)"
        );
        assert_eq!(p1["idempotency_key"], json!("w:a"));
        assert_eq!(
            p1["actor"],
            serde_json::to_value(ActorId::KernelDispatcher).unwrap()
        );

        let mut terminal = task("t", TaskStatus::Pending, &[], 0);
        terminal.kind = TaskKind::Terminal;
        terminal.goal = "make test".into();
        terminal.cwd = Some("/repo".into());
        let (kind, p) = build_worker_payload(&terminal).unwrap();
        assert_eq!(kind, "terminal-worker");
        assert_eq!(p["cmd"], json!("make test"));
        assert_eq!(p["cwd"], json!("/repo"));
    }

    // ----------------------------------------------------- inflight guard

    #[tokio::test]
    async fn inflight_guard_is_single_flight_and_releases_on_drop() {
        let map: Arc<DashMap<String, ()>> = Arc::new(DashMap::new());
        let g1 = InflightGuard::acquire(&map, "w:a").expect("first acquire");
        assert!(
            InflightGuard::acquire(&map, "w:a").is_none(),
            "second concurrent acquire must lose"
        );
        assert!(
            InflightGuard::acquire(&map, "w:b").is_some(),
            "other keys independent"
        );
        drop(g1);
        assert!(
            InflightGuard::acquire(&map, "w:a").is_some(),
            "slot frees on drop"
        );
    }
}
