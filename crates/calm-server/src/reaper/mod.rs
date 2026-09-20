use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calm_exec::SpawnCtx;
use calm_types::worker::{
    DeathVerdict, ExitInterpretation, Liveness, SessionMode, WorkerSession, WorkerSessionState,
};

use crate::db::prelude::*;
use crate::db::sqlite::{TaskReporter, status_detail_with_reason, task_fail_from_worker_tx};
use crate::db::write_with_actor_events_typed;
use crate::error::Result;
use crate::event::{Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::model::TrackLifecycle;
use crate::model::now_ms;
use crate::operation::workspace_lease::release_workspace_lease_for_card_repo;
use crate::provider_registry::WorkerProviderRegistry;
use crate::scheduler::{is_race_lost, race_lost_err};
use crate::state::WriteContext;
use crate::track_lifecycle::auto_transition_if_current_in_tx;

pub const DEFAULT_REAPER_RECONCILE_SECS: u64 = 30;

/// Pre-gate: a codex worker active within this window (or with a busy thread) is never reaped. Override with `NEIGE_REAPER_DEADLINE_SECS`.
pub const DEFAULT_REAPER_DEADLINE_SECS: u64 = 900;

/// After a daemon (re)connect, hold off `thread/read` pulls until the loaded-thread roster has stabilised. Override with `NEIGE_REAPER_REBUILD_GRACE_SECS`.
pub const DEFAULT_REAPER_REBUILD_GRACE_SECS: u64 = 300;

static REAPER_BOOT_DONE: AtomicBool = AtomicBool::new(false);

/// Resolve a positive seconds value from `var` (non-positive / garbage → `default`).
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
    /// Inactivity deadline in ms.
    deadline_ms: i64,
    /// Daemon-reconnect rebuild grace in ms.
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
            if let Some(card_id) = session.card_id.as_ref() {
                match crate::isolated_codex::lookup::is_isolated_card(
                    self.repo.as_ref(),
                    card_id.as_ref(),
                )
                .await
                {
                    Ok(true) => continue,
                    Err(error) => {
                        tracing::warn!(session_id=%session.id,%error,"reaper backend identity unavailable");
                        continue;
                    }
                    Ok(false) => {}
                }
            }
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
                    // A `starting` row exists BEFORE the spawn registers a PTY with the proc-supervisor, so `proc_running:false` here means 'not registered YET', not 'exited'. The spawn saga owns `starting` sessions: record the probe only.
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

                    // For a resumable provider a PTY teardown does NOT mean the codex thread died (a proc-supervisor restart empties the registry while the thread survives on the daemon), so only a positive `Dead` verdict from the death arbiter authorizes a reap.
                    if provider.session_mode() == SessionMode::Resumable {
                        // Cheap pre-gate: never costs an RPC. NULL `last_activity_ms` ⇒ `created_at_ms`, NOT `now`, which would make a never-active session look perpetually fresh.
                        let last = session.last_activity_ms.unwrap_or(session.created_at_ms);
                        // `idle` / `systemError` / `notLoaded` / `unknown` are NOT busy; they rely on the time pre-gate, which the same stamp refreshes.
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
                        // No `thread_id` ⇒ can't confirm death ⇒ no reap.
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
                            // Positively dead — fall through to the converge path shared with ephemeral.
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
                            // Converge BEFORE terminalizing so the path is re-drivable: terminalizing first and then crashing would drop the session from `sessions_nonterminal` while the task stayed `running` — a permanent stall. A mid-crash leaves the session active and re-probed next tick.
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
                            // Unreachable from the probe `-1` sentinel, but record the liveness so the session is not a silent skip if a provider ever produces one.
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
    /// The dead-ROOT convergence scan: same boot gate and reconcile loop as `sweep_all`; drives a positively-dead root's track `Draft|Planning → Failed`.
    /// The soundness predicate (never converge a live or merely just-created track) is enforced inside `dead_root_candidates`; this loop only emits.
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
                    track_id = %candidate.track_id,
                    lifecycle = candidate.lifecycle.as_db_str(),
                    error = %e,
                    "reaper: dead-root convergence failed; will retry next sweep"
                );
            }
        }
    }
}

/// The task-less dead-root emitter: a dead root has no task row, so there is NO `TaskFailed` — only `TrackLifecycleChanged{from → Failed}`, authored by `ActorId::KernelDispatcher`.
/// `auto_transition_if_current_in_tx` is a CAS on the current lifecycle; `None` means a live writer raced us and is treated as a race-loss (`Ok(())`).
pub(crate) async fn converge_dead_root(
    repo: &dyn Repo,
    events: &EventBus,
    write: &WriteContext,
    candidate: &crate::db::prelude::DeadRootCandidate,
) -> Result<()> {
    let track_id = candidate.track_id.clone();
    let from = candidate.lifecycle;
    let scope = EventScope::Track {
        track: candidate.track_id.clone(),
        area: candidate.area_id.clone(),
    };
    let agent_message = match from {
        TrackLifecycle::Draft => {
            "[auto] dead root: planner-harness start failed; track never advanced"
        }
        _ => "[auto] dead root: planner session lost mid-plan",
    }
    .to_string();

    let result = write_with_actor_events_typed::<(), _>(repo, None, events, write, move |tx| {
        Box::pin(async move {
            let Some(lifecycle_events) = auto_transition_if_current_in_tx(
                tx,
                &track_id,
                from,
                TrackLifecycle::Failed,
                &ActorId::KernelDispatcher,
                Some(agent_message),
            )
            .await?
            else {
                // Track already moved ⇒ race-lost; the outer match absorbs it into Ok(()) so no partial event batch lands.
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
    let Some(track) = repo.track_get(session.track_id.as_str()).await? else {
        release_reaped_worker_workspace_lease(repo, events, session).await?;
        return Ok(());
    };

    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let track_id = track.id.clone();
    // The kernel `TaskFailed` carries the provider's interpreted reason rather than the raw `-1` probe sentinel.
    let reason = reason.to_string();
    let result = write_with_actor_events_typed::<(), _>(repo, None, events, write, move |tx| {
        Box::pin(async move {
            // The `spawn-failed` classifier is knowingly wrong here (a reaped worker died at RUNTIME); correcting the vocabulary has its own consumers (`is_deferred_self_report`). The reason tail at least stops the row from lying silently.
            let rows = task_fail_from_worker_tx(
                tx,
                &task_id,
                track_id.as_str(),
                TaskReporter::Kernel,
                &status_detail_with_reason("spawn-failed", &reason),
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
                    details: None,
                    agent_message: None,
                },
            )];
            if let Some(auto_events) = auto_transition_if_current_in_tx(
                tx,
                &track_id,
                TrackLifecycle::Working,
                TrackLifecycle::Reviewing,
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
