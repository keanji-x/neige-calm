use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calm_exec::SpawnCtx;
use calm_types::worker::{
    DeathVerdict, ExitInterpretation, Liveness, SessionMode, WorkerSession, WorkerSessionState,
};

use crate::db::prelude::*;
use crate::db::sqlite::{TaskReporter, task_fail_from_worker_tx};
use crate::db::write_with_actor_events_typed;
use crate::error::Result;
use crate::event::{Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::model::WaveLifecycle;
use crate::model::now_ms;
use crate::operation::workspace_lease::release_workspace_lease_for_card_repo;
use crate::provider_registry::WorkerProviderRegistry;
use crate::scheduler::{is_race_lost, race_lost_err};
use crate::state::WriteContext;
use crate::wave_lifecycle::auto_transition_if_current_in_tx;

pub const DEFAULT_REAPER_RECONCILE_SECS: u64 = 30;

/// §1.1(d) pre-gate: a codex worker whose `last_activity_ms` is within this
/// window (or whose thread is busy) is never reaped — no arbiter RPC. Default
/// 15 min; override with `NEIGE_REAPER_DEADLINE_SECS`.
pub const DEFAULT_REAPER_DEADLINE_SECS: u64 = 900;

/// §1.3 rebuild grace: after a daemon (re)connect, hold off S2 `thread/read`
/// pulls until the loaded-thread roster has stabilised. Default 5 min;
/// override with `NEIGE_REAPER_REBUILD_GRACE_SECS`.
pub const DEFAULT_REAPER_REBUILD_GRACE_SECS: u64 = 300;

static REAPER_BOOT_DONE: AtomicBool = AtomicBool::new(false);

/// Resolve a positive seconds value from `var` (non-positive / garbage →
/// `default`), mirroring `Scheduler::reconcile_secs_from_env_var`.
fn reaper_secs_from_env_var(var: &str, default: u64) -> u64 {
    match std::env::var(var) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(n) if n > 0 => n,
            _ => default,
        },
        Err(_) => default,
    }
}

pub fn reaper_on_boot() {
    REAPER_BOOT_DONE.store(true, Ordering::SeqCst);
}

pub fn reaper_boot_completed() -> bool {
    REAPER_BOOT_DONE.load(Ordering::SeqCst)
}

pub fn reaper_disabled_from_env() -> bool {
    std::env::var_os("NEIGE_REAPER_DISABLED").is_some()
}

#[derive(Clone)]
pub struct Reaper {
    repo: Arc<dyn Repo>,
    providers: WorkerProviderRegistry,
    events: EventBus,
    write: WriteContext,
    /// §1.1(d) inactivity deadline in ms (from `NEIGE_REAPER_DEADLINE_SECS`).
    deadline_ms: i64,
    /// §1.3 daemon-reconnect rebuild grace in ms
    /// (from `NEIGE_REAPER_REBUILD_GRACE_SECS`).
    rebuild_grace_ms: i64,
}

impl Reaper {
    pub fn new(
        repo: Arc<dyn Repo>,
        providers: WorkerProviderRegistry,
        events: EventBus,
        write: WriteContext,
    ) -> Self {
        let deadline_ms =
            reaper_secs_from_env_var("NEIGE_REAPER_DEADLINE_SECS", DEFAULT_REAPER_DEADLINE_SECS)
                as i64
                * 1_000;
        let rebuild_grace_ms = reaper_secs_from_env_var(
            "NEIGE_REAPER_REBUILD_GRACE_SECS",
            DEFAULT_REAPER_REBUILD_GRACE_SECS,
        ) as i64
            * 1_000;
        Self {
            repo,
            providers,
            events,
            write,
            deadline_ms,
            rebuild_grace_ms,
        }
    }

    pub async fn sweep_all(&self) {
        if !reaper_boot_completed() {
            tracing::debug!(
                "reaper: liveness sweep skipped - boot backfill/recovery has not completed yet"
            );
            return;
        }

        let sessions = match self.repo.sessions_nonterminal().await {
            Ok(sessions) => sessions,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "reaper: failed to list non-terminal worker sessions"
                );
                return;
            }
        };

        for session in sessions {
            let Some(provider) = self.providers.get(session.provider) else {
                tracing::warn!(
                    session_id = %session.id,
                    provider = session.provider.as_db_str(),
                    "reaper: no worker provider registered for session"
                );
                continue;
            };

            let now = now_ms();
            let ctx = SpawnCtx::new(now);
            let liveness = match provider.probe_liveness(&session, &ctx).await {
                Ok(liveness) => liveness,
                Err(e) => {
                    tracing::warn!(
                        session_id = %session.id,
                        provider = session.provider.as_db_str(),
                        error = %e,
                        "reaper: provider liveness probe failed"
                    );
                    continue;
                }
            };

            match liveness {
                Liveness::Exited { evidence } => {
                    // P2 (spawn-window false-convergence), HOISTED above the
                    // session-mode branch so BOTH providers skip it: a
                    // `starting` row exists in `sessions_nonterminal` BEFORE the
                    // spawn registers a PTY with the proc-supervisor, so a
                    // supervisor `ProbeOk{proc_running:false}` here means "not
                    // spawned/registered YET", NOT "exited". A slow/stuck spawn
                    // outliving a reaper tick would otherwise be FALSELY
                    // converged while the spawn operation is still responsible
                    // for completing/failing it. The spawn saga owns `starting`
                    // sessions — record the probe as a T2 liveness observation
                    // (no terminalize, no event) and let the spawn op converge.
                    if session.state == WorkerSessionState::Starting {
                        tracing::debug!(
                            session_id = %session.id,
                            provider = session.provider.as_db_str(),
                            session_state = session.state.as_db_str(),
                            "reaper: session still in spawn/startup window; convergence owned by the spawn operation"
                        );
                        let observed = Liveness::Exited {
                            evidence: evidence.clone(),
                        };
                        if let Err(e) = self
                            .repo
                            .session_set_liveness(&session.id, &observed, now)
                            .await
                        {
                            tracing::warn!(
                                session_id = %session.id,
                                provider = session.provider.as_db_str(),
                                error = %e,
                                "reaper: failed to persist spawn-window Exited liveness observation"
                            );
                        }
                        continue;
                    }

                    // #741-3: un-defer the codex drive. A resumable provider
                    // (codex) whose PTY tore down does NOT itself mean the codex
                    // thread died (e.g. a proc-supervisor restart empties the
                    // registry while the thread survives on the separate
                    // daemon), so PTY `Exited` is necessary but NOT sufficient.
                    // Gate convergence on the death arbiter `confirm_durable_death`
                    // (§1.1 S1/S2) — only a positive `Dead` verdict authorizes
                    // a reap. The ephemeral path below is UNCHANGED.
                    if provider.session_mode() == SessionMode::Resumable {
                        // §1.1(d) cheap pre-gate: a recently-active or busy
                        // thread is never reaped, and never costs an RPC. NULL
                        // `last_activity_ms` ⇒ `created_at_ms` (NOT `now`, which
                        // would make a never-active session look perpetually
                        // fresh).
                        let last = session.last_activity_ms.unwrap_or(session.created_at_ms);
                        let busy = matches!(
                            session.last_thread_status.as_deref(),
                            Some("active" | "waitingOnUserInput" | "waitingOnApproval")
                        );
                        if busy || now.saturating_sub(last) <= self.deadline_ms {
                            tracing::debug!(
                                session_id = %session.id,
                                provider = session.provider.as_db_str(),
                                busy,
                                "reaper: resumable worker recently active / busy; pre-gate refuses reap"
                            );
                            let observed = Liveness::Exited {
                                evidence: evidence.clone(),
                            };
                            if let Err(e) = self
                                .repo
                                .session_set_liveness(&session.id, &observed, now)
                                .await
                            {
                                tracing::warn!(
                                    session_id = %session.id,
                                    provider = session.provider.as_db_str(),
                                    error = %e,
                                    "reaper: failed to persist resumable pre-gate liveness observation"
                                );
                            }
                            continue;
                        }
                        // No `thread_id` ⇒ nothing to `thread/read` ⇒ can't
                        // confirm death ⇒ no reap.
                        let Some(thread_id) = session.thread_id.as_deref() else {
                            tracing::debug!(
                                session_id = %session.id,
                                provider = session.provider.as_db_str(),
                                "reaper: resumable worker has no thread_id; cannot confirm death, no reap"
                            );
                            let observed = Liveness::Exited {
                                evidence: evidence.clone(),
                            };
                            if let Err(e) = self
                                .repo
                                .session_set_liveness(&session.id, &observed, now)
                                .await
                            {
                                tracing::warn!(
                                    session_id = %session.id,
                                    provider = session.provider.as_db_str(),
                                    error = %e,
                                    "reaper: failed to persist resumable no-thread liveness observation"
                                );
                            }
                            continue;
                        };
                        let connected = provider.daemon_connected_at_ms().unwrap_or(0);
                        let verdict = provider
                            .confirm_durable_death(thread_id, now, connected, self.rebuild_grace_ms)
                            .await;
                        match verdict {
                            // Positively dead — fall through to the EXISTING
                            // converge path (converge_dead_worker FIRST, then
                            // session_commit_exit), shared with ephemeral.
                            DeathVerdict::Dead => {}
                            // Alive / Unknown ⇒ NO reap; record T2 only.
                            _ => {
                                tracing::debug!(
                                    session_id = %session.id,
                                    provider = session.provider.as_db_str(),
                                    verdict = ?verdict,
                                    "reaper: arbiter did not confirm death; no reap"
                                );
                                let observed = Liveness::Exited {
                                    evidence: evidence.clone(),
                                };
                                if let Err(e) = self
                                    .repo
                                    .session_set_liveness(&session.id, &observed, now)
                                    .await
                                {
                                    tracing::warn!(
                                        session_id = %session.id,
                                        provider = session.provider.as_db_str(),
                                        error = %e,
                                        "reaper: failed to persist arbiter no-reap liveness observation"
                                    );
                                }
                                continue;
                            }
                        }
                    }

                    let verdict = match provider.interpret_exit(&session, &evidence, &ctx).await {
                        Ok(verdict) => verdict,
                        Err(e) => {
                            tracing::warn!(
                                session_id = %session.id,
                                provider = session.provider.as_db_str(),
                                exit_code = ?evidence.exit_code,
                                signal_killed = evidence.signal_killed,
                                error = %e,
                                "reaper: provider exit interpretation failed"
                            );
                            continue;
                        }
                    };

                    match verdict {
                        ExitInterpretation::Failed { reason } => {
                            // FIX 2 (A3): converge BEFORE terminalizing so the
                            // path is re-drivable. If we terminalized the
                            // session first and then crashed, the session
                            // would be dropped from `sessions_nonterminal`
                            // (never re-probed) while the task stayed
                            // `running` — a PERMANENT stall. By emitting the
                            // kernel `TaskFailed` + parking Working→Reviewing
                            // FIRST, a mid-crash leaves the session STILL
                            // ACTIVE → re-probed next tick → converge again
                            // (the in-tx `status IN (active)` CAS yields
                            // rows==0 → race-lost → Ok; `auto_transition`
                            // no-ops since the wave is already Reviewing) →
                            // then terminalize. Idempotent + re-drivable.
                            //
                            // FIX 3: carry the provider's Failed `reason` (it
                            // hides the `-1` probe sentinel and explains real
                            // non-zero/signal exits) into the TaskFailed event.
                            //
                            // §1.5 commit-CAS resume guard DEFERRED — neige does
                            // not wire codex worker resume today (§0.2), so the
                            // pull→commit resume TOCTOU is unreachable for
                            // workers; add `last_activity_ms <= :pull_ts` to
                            // `session_commit_exit_tx` when 8c wires resume.
                            if let Err(e) = converge_dead_worker(
                                self.repo.as_ref(),
                                &self.events,
                                &self.write,
                                &session,
                                &reason,
                            )
                            .await
                            {
                                tracing::warn!(
                                    session_id = %session.id,
                                    provider = session.provider.as_db_str(),
                                    error = %e,
                                    "reaper: dead-worker convergence failed"
                                );
                                // Leave the session active; next tick re-drives
                                // convergence before terminalizing.
                                continue;
                            }
                            match self
                                .repo
                                .session_commit_exit(
                                    &session.id,
                                    WorkerSessionState::Failed,
                                    now,
                                    evidence.exit_code,
                                    "failed",
                                )
                                .await
                            {
                                Ok(CommitExitOutcome::Committed(_)) => {}
                                Ok(CommitExitOutcome::Absorbed) => {
                                    // A live writer already terminalized this
                                    // session; convergence above was a no-op
                                    // race-loss. Nothing more to do.
                                    tracing::debug!(
                                        session_id = %session.id,
                                        provider = session.provider.as_db_str(),
                                        "reaper: exited session already terminalized by a live writer"
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        session_id = %session.id,
                                        provider = session.provider.as_db_str(),
                                        error = %e,
                                        "reaper: failed to commit exited session"
                                    );
                                }
                            }
                        }
                        ExitInterpretation::Completed
                        | ExitInterpretation::PreserveCard
                        | ExitInterpretation::ResumeEligible => {
                            // FIX 4 (P2a): these verdicts are unreachable from
                            // the probe `-1` sentinel (funnel/8c territory),
                            // but record the liveness so the session is not a
                            // silent skip if a provider ever produces one.
                            tracing::debug!(
                                session_id = %session.id,
                                provider = session.provider.as_db_str(),
                                verdict = ?verdict,
                                "reaper: exit verdict deferred to 8b-iii/8c; reaper probe should not produce this"
                            );
                            let observed = Liveness::Exited {
                                evidence: evidence.clone(),
                            };
                            if let Err(e) = self
                                .repo
                                .session_set_liveness(&session.id, &observed, now)
                                .await
                            {
                                tracing::warn!(
                                    session_id = %session.id,
                                    provider = session.provider.as_db_str(),
                                    error = %e,
                                    "reaper: failed to persist non-failed exit liveness observation"
                                );
                            }
                        }
                    }
                }
                liveness => {
                    if let Err(e) = self
                        .repo
                        .session_set_liveness(&session.id, &liveness, now)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session.id,
                            provider = session.provider.as_db_str(),
                            error = %e,
                            "reaper: failed to persist liveness observation"
                        );
                    }
                }
            }
        }
    }
}

impl Reaper {
    /// #741-4 (DR-2/DR-4/DR-5) — the dead-ROOT convergence scan. A sibling of
    /// [`Reaper::sweep_all`]: same boot gate (DR-5 — must not fire before the
    /// root backfill `0050` has settled), same reconcile loop. Drives a
    /// POSITIVELY-dead root's wave `Draft|Planning → Failed` via the DR-1
    /// kernel FSM edges. The soundness predicate (the CARDINAL SAFETY RULE:
    /// never converge a live or merely just-created wave) is enforced inside
    /// [`SessionRepo::dead_root_candidates`]; this loop only emits.
    pub async fn sweep_dead_roots(&self) {
        if !reaper_boot_completed() {
            tracing::debug!(
                "reaper: dead-root scan skipped - boot backfill/recovery has not completed yet"
            );
            return;
        }

        let candidates = match self.repo.dead_root_candidates().await {
            Ok(candidates) => candidates,
            Err(e) => {
                tracing::warn!(error = %e, "reaper: failed to scan for dead-root candidates");
                return;
            }
        };

        for candidate in candidates {
            if let Err(e) =
                converge_dead_root(self.repo.as_ref(), &self.events, &self.write, &candidate).await
            {
                tracing::warn!(
                    wave_id = %candidate.wave_id,
                    lifecycle = candidate.lifecycle.as_db_str(),
                    error = %e,
                    "reaper: dead-root convergence failed; will retry next sweep"
                );
            }
        }
    }
}

/// #741-4 (DR-3) — the task-less dead-root emitter. FRESH code, NOT a reuse of
/// `converge_dead_worker`'s no-op NULL `spawn_op_id` fall-through: a dead root
/// has no task row, so there is NO `TaskFailed` and NO task-status flip — only
/// the `WaveLifecycleChanged{from → Failed}` lifecycle event, authored by
/// `ActorId::KernelDispatcher` (cardless → unrestricted emit, no recorder gate;
/// DR-6).
///
/// Drives the edge via `auto_transition_if_current_in_tx`, which is a CAS on
/// the current lifecycle: if the wave already moved (a live writer raced us, or
/// the candidate read is stale), it returns `None` and we treat that as a
/// race-loss (`Ok(())`).
pub(crate) async fn converge_dead_root(
    repo: &dyn Repo,
    events: &EventBus,
    write: &WriteContext,
    candidate: &crate::db::prelude::DeadRootCandidate,
) -> Result<()> {
    let wave_id = candidate.wave_id.clone();
    let from = candidate.lifecycle;
    let scope = EventScope::Wave {
        wave: candidate.wave_id.clone(),
        cove: candidate.cove_id.clone(),
    };
    let agent_message = match from {
        WaveLifecycle::Draft => "[auto] dead root: spec-harness start failed; wave never advanced",
        _ => "[auto] dead root: planner session lost mid-plan",
    }
    .to_string();

    let result = write_with_actor_events_typed::<(), _>(repo, None, events, write, move |tx| {
        Box::pin(async move {
            let Some(lifecycle_events) = auto_transition_if_current_in_tx(
                tx,
                &wave_id,
                from,
                WaveLifecycle::Failed,
                &ActorId::KernelDispatcher,
                Some(agent_message),
            )
            .await?
            else {
                // Wave already moved (auto_transition no-op / current != from)
                // ⇒ race-lost. Return a race-lost error the outer match
                // absorbs into Ok(()) so no partial/empty event batch lands.
                return Err(race_lost_err());
            };
            let events = lifecycle_events
                .into_iter()
                .map(|event| (ActorId::KernelDispatcher, scope.clone(), event))
                .collect();
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

pub(crate) async fn converge_dead_worker(
    repo: &dyn Repo,
    events: &EventBus,
    write: &WriteContext,
    session: &WorkerSession,
    reason: &str,
) -> Result<()> {
    let Some(op_id) = session.spawn_op_id.as_deref() else {
        release_reaped_worker_workspace_lease(repo, events, session).await?;
        return Ok(());
    };
    let Some(task_id) = repo.operation_idempotency_key_by_id(op_id).await? else {
        release_reaped_worker_workspace_lease(repo, events, session).await?;
        return Ok(());
    };
    let Some(wave) = repo.wave_get(session.wave_id.as_str()).await? else {
        release_reaped_worker_workspace_lease(repo, events, session).await?;
        return Ok(());
    };

    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let wave_id = wave.id.clone();
    // FIX 3: the kernel `TaskFailed` carries the provider's interpreted Failed
    // reason (e.g. "terminal worker exited (outcome unknown; observed via
    // supervisor probe)") rather than the raw `-1` probe sentinel.
    let reason = reason.to_string();
    let result = write_with_actor_events_typed::<(), _>(repo, None, events, write, move |tx| {
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
                Some("[auto] worker died without reporting".to_string()),
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
        Ok(_) => {
            release_reaped_worker_workspace_lease(repo, events, session).await?;
            Ok(())
        }
        Err(e) if is_race_lost(&e) => {
            release_reaped_worker_workspace_lease(repo, events, session).await?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

async fn release_reaped_worker_workspace_lease(
    repo: &dyn Repo,
    events: &EventBus,
    session: &WorkerSession,
) -> Result<()> {
    if let Some(card_id) = session.card_id.as_ref() {
        release_workspace_lease_for_card_repo(repo, events, card_id.as_str()).await?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn reset_reaper_boot_gate_for_test() {
    REAPER_BOOT_DONE.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests;
