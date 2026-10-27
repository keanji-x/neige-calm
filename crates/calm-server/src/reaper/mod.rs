use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calm_exec::SpawnCtx;
use calm_types::worker::{
    DeathVerdict, ExitInterpretation, Liveness, SessionMode, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};

use crate::db::prelude::*;
use crate::db::sqlite::{
    TaskReporter, session_get_tx, status_detail_with_reason, task_fail_from_worker_tx, task_get_tx,
    track_find_tx, worker_op_targets_card_tx,
};
use crate::db::write_in_tx_typed;
use crate::error::Result;
use crate::event::{Event, EventBus, EventScope};
use crate::git_candidate::delivery::AttemptOutcome;
use crate::ids::ActorId;
use crate::model::now_ms;
use crate::operation::workspace_lease::{ReleaseDelivery, release_workspace_lease_for_card_tx};
use crate::provider_registry::WorkerProviderRegistry;
use crate::state::WriteContext;

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
    #[cfg(test)]
    after_probe: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
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
            #[cfg(test)]
            after_probe: None,
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
                        let verdict = provider
                            .confirm_durable_death(thread_id, now, self.rebuild_grace_ms)
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
                            #[cfg(test)]
                            if let Some((entered, resume)) = &self.after_probe {
                                entered.notify_one();
                                resume.notified().await;
                            }
                            match converge_dead_worker(
                                self.repo.as_ref(),
                                &self.events,
                                &self.write,
                                &session,
                                &reason,
                            )
                            .await
                            {
                                Ok(true) => {}
                                Ok(false) => continue,
                                Err(e) => {
                                    tracing::warn!(
                                        session_id = %session.id,
                                        provider = session.provider.as_db_str(),
                                        error = %e,
                                        "reaper: dead-worker convergence failed"
                                    );
                                    // Leave active for the next tick to re-drive convergence.
                                    continue;
                                }
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
                                    // session after convergence. Nothing more to do.
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

/// Returns false for superseded discovery; the caller must leave that session alone.
pub(crate) async fn converge_dead_worker(
    repo: &dyn Repo,
    events: &EventBus,
    write: &WriteContext,
    session: &WorkerSession,
    reason: &str,
) -> Result<bool> {
    let session_id = session.id.clone();
    let reason = reason.to_owned();
    let write = write.clone();
    let (applied, ids, pending) = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move {
            let (applied, pending) = converge_dead_worker_tx(tx, &session_id, reason).await?;
            let ids = if pending.is_empty() {
                Vec::new()
            } else {
                crate::db::sqlite::append_actor_events_in_tx(tx, None, &write, &pending)
                    .await?
                    .0
            };
            Ok((applied, ids, pending))
        })
    })
    .await?;
    crate::db::sqlite::broadcast_actor_events(events, &ids, pending);
    Ok(applied)
}

async fn converge_dead_worker_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session_id: &WorkerSessionId,
    reason: String,
) -> Result<(bool, Vec<(ActorId, EventScope, Event)>)> {
    // Discovery and provider probes are hints. Re-read under the write lock before
    // resolving any task binding or selecting the card's latest lease (WR1/WR4).
    let Some(session) = session_get_tx(tx, session_id).await? else {
        return Ok((false, Vec::new()));
    };
    let current = if let Some(card_id) = session.card_id.as_ref() {
        sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM cards WHERE id = ?1 AND session_id = ?2 AND track_id = ?3)",
    )
    .bind(card_id.as_str())
    .bind(session.id.as_str())
    .bind(session.track_id.as_str())
    .fetch_one(&mut **tx)
    .await?
    } else {
        true
    };
    if session.state.is_terminal() || !current {
        return Ok((false, Vec::new()));
    }

    let Some(card_id) = session.card_id.as_ref() else {
        return Ok((true, Vec::new()));
    };
    let task_id = if let Some(op_id) = session.spawn_op_id.as_deref() {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT idempotency_key FROM operations WHERE id = ?1",
        )
        .bind(op_id)
        .fetch_optional(&mut **tx)
        .await?
        .flatten()
    } else {
        None
    };
    let track = track_find_tx(tx, session.track_id.as_str()).await?;
    let mut events = Vec::new();
    let mut delivery = ReleaseDelivery::CommitAsTaskEnded;
    if let (Some(task_id), Some(track)) = (task_id, track) {
        let owns_key = worker_op_targets_card_tx(tx, &task_id, card_id.as_str()).await?;
        // The legacy probe positively confirmed death. The card guard includes
        // the scheduler's op-target proof for the first, unstamped report window.
        let rows = task_fail_from_worker_tx(
            tx,
            &task_id,
            track.id.as_str(),
            TaskReporter::Card {
                card_id: card_id.as_str(),
                owns_key,
            },
            &status_detail_with_reason("spawn-failed", &reason),
            now_ms(),
        )
        .await?;
        if rows == 0
            && let Some(task) = task_get_tx(tx, &task_id).await?
            && (!task.status.is_terminal()
                || task.track_id != track.id.as_str()
                || match task.worker_card_id.as_deref() {
                    Some(owner) => owner != card_id.as_str(),
                    None => !owns_key,
                })
        {
            // A task owned elsewhere is no authority to release this card's
            // lease. Only terminal races with a valid binding may converge.
            return Ok((true, Vec::new()));
        }
        if rows != 0 {
            delivery = ReleaseDelivery::Commit(AttemptOutcome::Failed);
            events.push((
                ActorId::KernelDispatcher,
                EventScope::Track {
                    track: track.id,
                    area: track.area_id,
                },
                Event::TaskFailed {
                    notification: calm_types::event::PlannerNotification::Notify,
                    idempotency_key: task_id,
                    reason,
                    details: None,
                    agent_message: None,
                },
            ));
        }
    }
    events.extend(release_workspace_lease_for_card_tx(tx, card_id.as_str(), delivery).await?);
    Ok((true, events))
}

#[cfg(test)]
pub(crate) fn reset_reaper_boot_gate_for_test() {
    REAPER_BOOT_DONE.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod fence_tests;
#[cfg(test)]
mod liveness_tests;
#[cfg(test)]
mod test_support;
