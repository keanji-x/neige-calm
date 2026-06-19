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
        return Ok(());
    };
    let Some(task_id) = repo.operation_idempotency_key_by_id(op_id).await? else {
        return Ok(());
    };
    let Some(wave) = repo.wave_get(session.wave_id.as_str()).await? else {
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
mod tests {
    use super::*;

    use crate::card_role_cache::CardRoleCache;
    use calm_exec::WorkerProvider;
    use calm_truth::db::RepoEventWrite;
    use calm_truth::db::RepoSyncDomainRaw;
    use calm_truth::db::sqlite::{SqlxRepo, begin_immediate_tx, session_insert_tx, task_insert_tx};
    use calm_truth::session_repo::SessionRepo;
    use calm_truth_test_harness::FakeProvider;
    use calm_types::ids::{CardId, WaveId};
    use calm_types::worker::{
        ExitEvidence, ExitSource, LivenessTag, SessionMode, WorkerContract, WorkerProviderKind,
        WorkerSession, WorkerSessionId, WorkerSessionState,
    };
    use serde_json::json;

    use crate::model::{
        Card, NewCard, NewCove, NewWave, RequestTheme, Task, TaskKind, TaskStatus, WaveLifecycle,
        new_id,
    };
    use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
    use crate::state::WriteContext;
    use crate::wave_cove_cache::WaveCoveCache;

    static REAPER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn seeded_repo() -> (Arc<SqlxRepo>, WaveId) {
        let repo = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        let cove = RepoSyncDomainRaw::cove_create(
            repo.as_ref(),
            NewCove {
                name: "reaper-test".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .expect("seed cove");
        let wave = RepoSyncDomainRaw::wave_create(
            repo.as_ref(),
            NewWave {
                cove_id: cove.id,
                title: "reaper-test".into(),
                sort: None,
                cwd: "/tmp".into(),
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            },
        )
        .await
        .expect("seed wave");
        (repo, wave.id)
    }

    fn session(id: &str, wave_id: WaveId, created_at_ms: i64) -> WorkerSession {
        WorkerSession {
            id: WorkerSessionId::from(id),
            wave_id,
            provider: WorkerProviderKind::Terminal,
            mode: SessionMode::Ephemeral,
            contract: WorkerContract::Executor,
            parent_session_id: None,
            requester_session_id: None,
            state: WorkerSessionState::Running,
            mcp_token_hash: None,
            thread_id: None,
            agent_session_id: None,
            active_turn_id: None,
            terminal_run_id: None,
            card_id: Some(CardId(format!("card-{id}"))),
            handle_state_json: None,
            liveness: LivenessTag::Unknown,
            liveness_probed_at_ms: None,
            exit_code: None,
            exit_interpretation: None,
            spawn_op_id: None,
            last_activity_ms: None,
            last_thread_status: None,
            created_at_ms,
            updated_at_ms: created_at_ms,
            completed_at_ms: None,
        }
    }

    async fn insert_session(repo: &SqlxRepo, mut session: WorkerSession) -> Card {
        let card = RepoSyncDomainRaw::card_create(
            repo,
            NewCard {
                wave_id: session.wave_id.clone(),
                kind: "terminal".into(),
                sort: None,
                payload: json!({}),
            },
        )
        .await
        .expect("seed runtime card");
        let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
        let session_id = session.id.clone();
        session.card_id = Some(CardId(card.id.to_string()));
        session_insert_tx(&mut tx, session)
            .await
            .expect("insert session");
        sqlx::query("UPDATE cards SET session_id = ?1 WHERE id = ?2")
            .bind(session_id.as_str())
            .bind(card.id.as_str())
            .execute(&mut *tx)
            .await
            .expect("link card session");
        tx.commit().await.expect("commit tx");
        card
    }

    fn exited_liveness() -> Liveness {
        Liveness::Exited {
            evidence: ExitEvidence {
                exit_code: Some(7),
                signal_killed: false,
                observed_at_ms: 123,
                source: ExitSource::Probe,
            },
        }
    }

    fn registry(fake: Arc<FakeProvider>) -> WorkerProviderRegistry {
        registry_for(WorkerProviderKind::Terminal, fake)
    }

    fn registry_for(kind: WorkerProviderKind, fake: Arc<FakeProvider>) -> WorkerProviderRegistry {
        WorkerProviderRegistry::from_entries([(kind, fake as Arc<dyn WorkerProvider>)])
    }

    async fn write_context(repo: &SqlxRepo) -> WriteContext {
        let role_cache = CardRoleCache::new();
        let wave_cove_cache = WaveCoveCache::new();
        repo.seed_card_role_cache(&role_cache)
            .await
            .expect("seed card role cache");
        repo.seed_wave_cove_cache(&wave_cove_cache)
            .await
            .expect("seed wave cove cache");
        WriteContext::new(role_cache, wave_cove_cache)
    }

    async fn set_wave_lifecycle(repo: &SqlxRepo, wave_id: &WaveId, lifecycle: WaveLifecycle) {
        sqlx::query("UPDATE waves SET lifecycle = ?1 WHERE id = ?2")
            .bind(lifecycle.as_db_str())
            .bind(wave_id.as_str())
            .execute(repo.pool())
            .await
            .expect("set wave lifecycle");
    }

    async fn insert_task(repo: &SqlxRepo, wave_id: &WaveId, key: &str, status: TaskStatus) -> Task {
        let now = now_ms();
        let task = Task {
            id: format!("{}:{key}", wave_id.as_str()),
            wave_id: wave_id.as_str().to_string(),
            key: key.into(),
            kind: TaskKind::Terminal,
            goal: "test worker".into(),
            context_json: "null".into(),
            acceptance_criteria: None,
            cwd: None,
            depends_on_json: "[]".into(),
            priority: 0,
            gate_json: None,
            status,
            status_detail: None,
            worker_card_id: None,
            gate_result_json: None,
            gate_attempt: 0,
            gate_pid: None,
            gate_pid_starttime: None,
            gate_pid_boot_id: None,
            created_at_ms: now,
            updated_at_ms: now,
            finished_at_ms: None,
        };
        let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
        task_insert_tx(&mut tx, &task).await.expect("insert task");
        tx.commit().await.expect("commit tx");
        task
    }

    async fn insert_spawn_operation(
        repo: &SqlxRepo,
        task_id: Option<&str>,
        target_card_id: Option<&str>,
    ) -> String {
        let op_repo = SqlxOperationRepo::new(repo.pool().clone());
        let op_id = op_repo
            .insert_operation(
                "terminal-worker",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: task_id.map(str::to_string),
                    payload_hash: format!("hash-{}", new_id()),
                },
                json!({
                    "actor": ActorId::KernelDispatcher,
                    "kind": "terminal-worker-test"
                }),
            )
            .await
            .expect("insert operation");
        if let Some(card_id) = target_card_id {
            sqlx::query(
                "UPDATE operations SET target_type = 'card', target_id = ?1, target_json = ?2 \
                 WHERE id = ?3",
            )
            .bind(card_id)
            .bind(json!({ "type": "card", "id": card_id }).to_string())
            .bind(&op_id)
            .execute(repo.pool())
            .await
            .expect("stamp operation target");
        }
        op_id
    }

    async fn acquire_test_workspace_lease(
        repo: &SqlxRepo,
        card_id: &str,
        wave_id: &WaveId,
        lease_owner: &str,
    ) -> (String, String) {
        let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
        let (lease, _event) = crate::operation::workspace_lease::acquire_workspace_lease_tx(
            &mut tx,
            card_id,
            wave_id.as_str(),
            lease_owner,
        )
        .await
        .expect("acquire workspace lease");
        tx.commit().await.expect("commit lease");
        (lease.lease_id, lease.path)
    }

    async fn task_failed_events(repo: &SqlxRepo, task_id: &str) -> Vec<Event> {
        RepoEventWrite::events_since(repo, 0, None)
            .await
            .expect("events")
            .into_iter()
            .filter_map(|(_id, _version, _scope, event)| match &event {
                Event::TaskFailed {
                    idempotency_key, ..
                } if idempotency_key == task_id => Some(event),
                _ => None,
            })
            .collect()
    }

    async fn lifecycle_changes(repo: &SqlxRepo, wave_id: &WaveId) -> Vec<Event> {
        RepoEventWrite::events_since(repo, 0, None)
            .await
            .expect("events")
            .into_iter()
            .filter_map(|(_id, _version, scope, event)| {
                if scope.wave_id() != Some(wave_id) {
                    return None;
                }
                matches!(event, Event::WaveLifecycleChanged { .. }).then_some(event)
            })
            .collect()
    }

    // ----- #741-4 dead-root convergence test helpers -----------------------

    /// Insert a `spec-harness-start` operation for `wave_id` and stamp its
    /// terminal `phase` (DR-4's positive dead signal keys on `phase='failed'`).
    /// The payload carries `wave_id` at top level — the immutable op→wave link
    /// `dead_root_candidates` queries via `json_extract(payload_json,
    /// '$.wave_id')`.
    async fn insert_spec_harness_start_op(repo: &SqlxRepo, wave_id: &WaveId, phase: &str) {
        let op_repo = SqlxOperationRepo::new(repo.pool().clone());
        let op_id = op_repo
            .insert_operation(
                "spec-harness-start",
                OperationKey {
                    operation_key: new_id(),
                    idempotency_key: None,
                    payload_hash: format!("hash-{}", new_id()),
                },
                json!({
                    "actor": ActorId::KernelDispatcher,
                    "wave_id": wave_id.as_str(),
                    "spec_card_id": "spec-card-1",
                    "cwd": "/tmp",
                }),
            )
            .await
            .expect("insert spec-harness-start operation");
        // `insert_operation` always lands `phase='pending'`; advance to the
        // requested terminal phase (mirrors `mark_failed`, which sets `phase`
        // and a completed timestamp without touching target columns).
        sqlx::query("UPDATE operations SET phase = ?1, completed_at_ms = ?2 WHERE id = ?3")
            .bind(phase)
            .bind(if matches!(phase, "failed" | "succeeded") {
                Some(now_ms())
            } else {
                None
            })
            .bind(&op_id)
            .execute(repo.pool())
            .await
            .expect("stamp operation phase");
    }

    /// Insert a planner-contract session in `state` and (optionally) mark it the
    /// wave's `root_session_id`.
    async fn insert_planner_session(
        repo: &SqlxRepo,
        id: &str,
        wave_id: &WaveId,
        state: WorkerSessionState,
        mark_root: bool,
    ) {
        let mut sess = session(id, wave_id.clone(), 1);
        sess.provider = WorkerProviderKind::Codex;
        sess.mode = SessionMode::Resumable;
        sess.contract = WorkerContract::Planner;
        sess.state = state;
        let wave_id = wave_id.clone();
        let session_id = WorkerSessionId::from(id);
        crate::db::write_in_tx_typed(repo, move |tx| {
            Box::pin(async move {
                session_insert_tx(tx, sess).await?;
                if mark_root {
                    calm_truth::db::sqlite::session_mark_wave_root_tx(tx, &wave_id, &session_id)
                        .await?;
                }
                Ok(())
            })
        })
        .await
        .expect("insert planner session");
    }

    async fn wave_lifecycle_now(repo: &SqlxRepo, wave_id: &WaveId) -> WaveLifecycle {
        repo.wave_get(wave_id.as_str())
            .await
            .expect("wave get")
            .expect("wave exists")
            .lifecycle
    }

    /// DR-4 failed-start: a `Draft` wave whose `spec-harness-start` op resolved
    /// to `phase='failed'`, with NO active planner session, converges
    /// `Draft → Failed` — exactly one `WaveLifecycleChanged` (KernelDispatcher),
    /// and NO `TaskFailed` (a dead root has no task row).
    #[tokio::test]
    async fn sweep_dead_roots_failed_start_draft_converges_to_failed() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        // Wave starts Draft (default); record a FAILED start-op for it.
        assert_eq!(
            wave_lifecycle_now(&repo, &wave_id).await,
            WaveLifecycle::Draft
        );
        insert_spec_harness_start_op(&repo, &wave_id, "failed").await;

        let fake = Arc::new(FakeProvider::new());
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry(fake),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_dead_roots().await;

        assert_eq!(
            wave_lifecycle_now(&repo, &wave_id).await,
            WaveLifecycle::Failed,
            "failed-start Draft wave must converge to Failed"
        );
        let changes = lifecycle_changes(&repo, &wave_id).await;
        assert_eq!(changes.len(), 1, "exactly one lifecycle change");
        match &changes[0] {
            Event::WaveLifecycleChanged { from, to, .. } => {
                assert_eq!(*from, WaveLifecycle::Draft);
                assert_eq!(*to, WaveLifecycle::Failed);
            }
            other => panic!("expected lifecycle change, got {other:?}"),
        }
        // No task row, so no TaskFailed event anywhere.
        let task_failed = RepoEventWrite::events_since(repo.as_ref(), 0, None)
            .await
            .expect("events")
            .into_iter()
            .filter(|(_id, _v, _s, e)| matches!(e, Event::TaskFailed { .. }))
            .count();
        assert_eq!(task_failed, 0, "dead-root convergence emits no TaskFailed");

        reset_reaper_boot_gate_for_test();
    }

    /// DR-4 SAFETY (the false-converge guard): a fresh `Draft` wave whose
    /// start-op is PENDING (or SUCCEEDED, or absent) is NOT a positive dead
    /// signal — it must stay `Draft`.
    #[tokio::test]
    async fn sweep_dead_roots_draft_pending_or_succeeded_or_absent_start_op_not_converged() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        // (a) pending start-op
        let (repo_pending, wave_pending) = seeded_repo().await;
        insert_spec_harness_start_op(&repo_pending, &wave_pending, "pending").await;
        // (b) succeeded start-op (the wave hasn't advanced past Draft yet, but
        //     the start succeeded — definitely not dead).
        let (repo_succeeded, wave_succeeded) = seeded_repo().await;
        insert_spec_harness_start_op(&repo_succeeded, &wave_succeeded, "succeeded").await;
        // (c) NO start-op row at all (just-created / in-flight — absence is
        //     ambiguous, must NOT converge).
        let (repo_absent, wave_absent) = seeded_repo().await;

        for (repo, wave_id, label) in [
            (repo_pending, wave_pending, "pending"),
            (repo_succeeded, wave_succeeded, "succeeded"),
            (repo_absent, wave_absent, "absent"),
        ] {
            let fake = Arc::new(FakeProvider::new());
            let repo_dyn: Arc<dyn Repo> = repo.clone();
            let reaper = Reaper::new(
                repo_dyn,
                registry(fake),
                EventBus::new(),
                write_context(&repo).await,
            );

            reaper_on_boot();
            reaper.sweep_dead_roots().await;

            assert_eq!(
                wave_lifecycle_now(&repo, &wave_id).await,
                WaveLifecycle::Draft,
                "Draft wave with {label} start-op must NOT converge (false-converge guard)"
            );
            assert_eq!(
                lifecycle_changes(&repo, &wave_id).await.len(),
                0,
                "no lifecycle change for {label} start-op"
            );
        }

        reset_reaper_boot_gate_for_test();
    }

    /// DR-4 latest-start-op guard (the stale-failed-plus-newer-retry hole):
    /// start/reset re-submit `spec-harness-start` with a FRESH op id, so a
    /// Draft wave can carry a STALE `failed` start-op AND a NEWER retry
    /// (`pending` or `succeeded`) start-op simultaneously. During the retry's
    /// setup window the planner session is not yet created, so the
    /// `no_active_planner` guard is momentarily true — convergence must still
    /// be refused because the LATEST start-op is non-failed. Keying on the
    /// most-recent start-op (max `rowid`) closes the false-converge hole.
    #[tokio::test]
    async fn sweep_dead_roots_stale_failed_plus_newer_retry_start_op_not_converged() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        // (a) STALE failed start-op, then a NEWER pending retry start-op
        //     (retry in flight, planner session not yet created).
        let (repo_pending, wave_pending) = seeded_repo().await;
        insert_spec_harness_start_op(&repo_pending, &wave_pending, "failed").await;
        insert_spec_harness_start_op(&repo_pending, &wave_pending, "pending").await;
        // (b) STALE failed start-op, then a NEWER succeeded retry start-op
        //     (start ultimately succeeded — definitely not dead).
        let (repo_succeeded, wave_succeeded) = seeded_repo().await;
        insert_spec_harness_start_op(&repo_succeeded, &wave_succeeded, "failed").await;
        insert_spec_harness_start_op(&repo_succeeded, &wave_succeeded, "succeeded").await;

        for (repo, wave_id, label) in [
            (repo_pending, wave_pending, "newer-pending"),
            (repo_succeeded, wave_succeeded, "newer-succeeded"),
        ] {
            assert_eq!(
                wave_lifecycle_now(&repo, &wave_id).await,
                WaveLifecycle::Draft
            );
            let fake = Arc::new(FakeProvider::new());
            let repo_dyn: Arc<dyn Repo> = repo.clone();
            let reaper = Reaper::new(
                repo_dyn,
                registry(fake),
                EventBus::new(),
                write_context(&repo).await,
            );

            reaper_on_boot();
            reaper.sweep_dead_roots().await;

            assert_eq!(
                wave_lifecycle_now(&repo, &wave_id).await,
                WaveLifecycle::Draft,
                "stale-failed + {label} retry start-op must NOT converge \
                 (latest start-op is non-failed)"
            );
            assert_eq!(
                lifecycle_changes(&repo, &wave_id).await.len(),
                0,
                "no lifecycle change for stale-failed + {label} retry"
            );
        }

        reset_reaper_boot_gate_for_test();
    }

    /// DR-4 mid-respawn exclusion: a Draft (failed start-op) OR Planning
    /// (NULL root) wave that has an ACTIVE planner-contract session is NOT
    /// converged — a respawn is in flight.
    #[tokio::test]
    async fn sweep_dead_roots_active_planner_session_excludes_convergence() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        // Draft + failed start-op, but a fresh planner session is `running`.
        let (repo_draft, wave_draft) = seeded_repo().await;
        insert_spec_harness_start_op(&repo_draft, &wave_draft, "failed").await;
        insert_planner_session(
            &repo_draft,
            "planner-respawn-draft",
            &wave_draft,
            WorkerSessionState::Running,
            false,
        )
        .await;

        // Planning + NULL root, but a planner session is `starting` (respawn).
        let (repo_planning, wave_planning) = seeded_repo().await;
        set_wave_lifecycle(&repo_planning, &wave_planning, WaveLifecycle::Planning).await;
        insert_planner_session(
            &repo_planning,
            "planner-respawn-planning",
            &wave_planning,
            WorkerSessionState::Starting,
            false,
        )
        .await;

        for (repo, wave_id, from) in [
            (repo_draft, wave_draft, WaveLifecycle::Draft),
            (repo_planning, wave_planning, WaveLifecycle::Planning),
        ] {
            let fake = Arc::new(FakeProvider::new());
            let repo_dyn: Arc<dyn Repo> = repo.clone();
            let reaper = Reaper::new(
                repo_dyn,
                registry(fake),
                EventBus::new(),
                write_context(&repo).await,
            );

            reaper_on_boot();
            reaper.sweep_dead_roots().await;

            assert_eq!(
                wave_lifecycle_now(&repo, &wave_id).await,
                from,
                "{from:?} wave with an ACTIVE planner session must NOT converge (mid-respawn)"
            );
            assert_eq!(lifecycle_changes(&repo, &wave_id).await.len(), 0);
        }

        reset_reaper_boot_gate_for_test();
    }

    /// DR-4 lost-root: a `Planning` wave whose root session is TERMINAL
    /// (failed) with no active planner session converges `Planning → Failed`.
    #[tokio::test]
    async fn sweep_dead_roots_lost_root_terminal_session_planning_converges() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Planning).await;
        // Root session exists but is TERMINAL (Failed) — the worker reaper
        // already terminalized it (S1/S2 for codex). No active planner.
        insert_planner_session(
            &repo,
            "planner-dead-root",
            &wave_id,
            WorkerSessionState::Failed,
            true,
        )
        .await;

        let fake = Arc::new(FakeProvider::new());
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry(fake),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_dead_roots().await;

        assert_eq!(
            wave_lifecycle_now(&repo, &wave_id).await,
            WaveLifecycle::Failed,
            "Planning wave with a terminal root + no active planner must converge to Failed"
        );
        let changes = lifecycle_changes(&repo, &wave_id).await;
        assert_eq!(changes.len(), 1);
        match &changes[0] {
            Event::WaveLifecycleChanged { from, to, .. } => {
                assert_eq!(*from, WaveLifecycle::Planning);
                assert_eq!(*to, WaveLifecycle::Failed);
            }
            other => panic!("expected lifecycle change, got {other:?}"),
        }

        reset_reaper_boot_gate_for_test();
    }

    /// DR-4 lost-root NULL: a `Planning` wave whose `root_session_id IS NULL`
    /// with no active planner session converges `Planning → Failed`.
    #[tokio::test]
    async fn sweep_dead_roots_lost_root_null_planning_converges() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Planning).await;
        // No root session at all, no active planner — a lost root.

        let fake = Arc::new(FakeProvider::new());
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry(fake),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_dead_roots().await;

        assert_eq!(
            wave_lifecycle_now(&repo, &wave_id).await,
            WaveLifecycle::Failed,
            "Planning wave with NULL root + no active planner must converge to Failed"
        );
        assert_eq!(lifecycle_changes(&repo, &wave_id).await.len(), 1);

        reset_reaper_boot_gate_for_test();
    }

    /// DR-5 boot gate: `sweep_dead_roots` no-ops until `reaper_on_boot`.
    #[tokio::test]
    async fn sweep_dead_roots_noops_until_reaper_on_boot_opens_gate() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        // A genuinely-dead failed-start root that WOULD converge post-boot.
        insert_spec_harness_start_op(&repo, &wave_id, "failed").await;

        let fake = Arc::new(FakeProvider::new());
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry(fake),
            EventBus::new(),
            write_context(&repo).await,
        );

        // Gate closed: must NOT converge.
        reaper.sweep_dead_roots().await;
        assert_eq!(
            wave_lifecycle_now(&repo, &wave_id).await,
            WaveLifecycle::Draft,
            "dead-root scan must no-op before boot gate opens"
        );
        assert_eq!(lifecycle_changes(&repo, &wave_id).await.len(), 0);

        // Gate open: now it converges.
        reaper_on_boot();
        reaper.sweep_dead_roots().await;
        assert_eq!(
            wave_lifecycle_now(&repo, &wave_id).await,
            WaveLifecycle::Failed,
            "dead-root scan converges once the boot gate opens"
        );
        assert_eq!(lifecycle_changes(&repo, &wave_id).await.len(), 1);

        reset_reaper_boot_gate_for_test();
    }

    #[tokio::test]
    async fn sweep_records_non_exit_liveness_and_terminals_exited_without_spawn_op() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        for (idx, id) in ["ws-alive", "ws-idle", "ws-unknown", "ws-exited"]
            .into_iter()
            .enumerate()
        {
            insert_session(&repo, session(id, wave_id.clone(), idx as i64 + 1)).await;
        }

        let fake = Arc::new(FakeProvider::new().with_probe_script([
            Liveness::Alive {
                active_turn_id: Some("turn-1".into()),
            },
            Liveness::Idle,
            Liveness::Unknown { since_ms: 99 },
            exited_liveness(),
        ]));
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry(fake.clone()),
            EventBus::new(),
            write_context(&repo).await,
        );
        let before_events = RepoEventWrite::events_since(repo.as_ref(), 0, None)
            .await
            .expect("events before");

        reaper_on_boot();
        reaper.sweep_all().await;

        assert_eq!(fake.probe_call_count(), 4);
        assert_eq!(
            RepoEventWrite::events_since(repo.as_ref(), 0, None)
                .await
                .expect("events after")
                .len(),
            before_events.len(),
            "exited reaper session without spawn_op_id must not emit task events"
        );

        for (id, tag) in [
            ("ws-alive", LivenessTag::Alive),
            ("ws-idle", LivenessTag::Idle),
            ("ws-unknown", LivenessTag::Unknown),
        ] {
            let row = repo
                .session_get(&WorkerSessionId::from(id))
                .await
                .expect("session get")
                .expect("session exists");
            assert_eq!(row.liveness, tag, "{id} liveness tag");
            assert!(
                row.liveness_probed_at_ms.is_some(),
                "{id} liveness_probed_at_ms"
            );
            assert_eq!(
                row.state,
                WorkerSessionState::Running,
                "{id} state must not transition"
            );
            assert_eq!(row.exit_code, None, "{id} exit_code untouched");
            assert_eq!(
                row.exit_interpretation, None,
                "{id} exit_interpretation untouched"
            );
        }

        let exited = repo
            .session_get(&WorkerSessionId::from("ws-exited"))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(exited.liveness, LivenessTag::Exited);
        assert!(exited.liveness_probed_at_ms.is_some());
        assert_eq!(exited.state, WorkerSessionState::Failed);
        assert_eq!(exited.exit_code, Some(7));
        assert_eq!(exited.exit_interpretation.as_deref(), Some("failed"));
        assert!(exited.completed_at_ms.is_some());

        reset_reaper_boot_gate_for_test();
    }

    #[tokio::test]
    async fn sweep_exited_failed_converges_dead_worker_task_and_parks_reviewing() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Working).await;
        let task = insert_task(&repo, &wave_id, "dead-worker", TaskStatus::Running).await;
        let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
        let mut worker = session("ws-dead-worker", wave_id.clone(), 1);
        worker.spawn_op_id = Some(op_id);
        insert_session(&repo, worker).await;

        let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
        let events = EventBus::new();
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry(fake.clone()),
            events,
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_all().await;

        assert_eq!(fake.probe_call_count(), 1);
        let worker = repo
            .session_get(&WorkerSessionId::from("ws-dead-worker"))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(worker.state, WorkerSessionState::Failed);
        assert_eq!(worker.liveness, LivenessTag::Exited);
        assert_eq!(worker.exit_code, Some(7));
        assert_eq!(worker.exit_interpretation.as_deref(), Some("failed"));

        let task_row = repo
            .task_get(&task.id)
            .await
            .expect("task get")
            .expect("task exists");
        assert_eq!(task_row.status, TaskStatus::Failed);
        assert_eq!(task_row.status_detail.as_deref(), Some("spawn-failed"));

        let failed = task_failed_events(&repo, &task.id).await;
        assert_eq!(failed.len(), 1);
        match &failed[0] {
            Event::TaskFailed {
                idempotency_key,
                reason,
                agent_message,
            } => {
                assert_eq!(idempotency_key, &task.id);
                // FIX 3: the provider's interpreted reason flows through, not
                // the kernel's old `"exit Some(..)"` format. The probe-sourced
                // evidence hides the exit sentinel behind "outcome unknown".
                assert!(
                    reason.contains("outcome unknown") && reason.contains("supervisor probe"),
                    "expected provider reason, got {reason:?}"
                );
                assert!(!reason.contains("exit Some("));
                assert_eq!(agent_message, &None);
            }
            other => panic!("expected task.failed, got {other:?}"),
        }

        let changes = lifecycle_changes(&repo, &wave_id).await;
        assert_eq!(changes.len(), 1);
        match &changes[0] {
            Event::WaveLifecycleChanged { from, to, .. } => {
                assert_eq!(*from, WaveLifecycle::Working);
                assert_eq!(*to, WaveLifecycle::Reviewing);
            }
            other => panic!("expected lifecycle change, got {other:?}"),
        }
        let wave = repo
            .wave_get(wave_id.as_str())
            .await
            .expect("wave get")
            .expect("wave exists");
        assert_eq!(wave.lifecycle, WaveLifecycle::Reviewing);

        reset_reaper_boot_gate_for_test();
    }

    /// #741-3 (a): a CODEX (`SessionMode::Resumable`) session observed `Exited`
    /// whose death arbiter returns `Dead` (with a stale `last_activity_ms` so
    /// the §1.1(d) pre-gate lets it through) MUST converge — mirroring the
    /// ephemeral convergence: cardless `TaskFailed`, park Working→Reviewing,
    /// session terminalized.
    #[tokio::test]
    async fn sweep_resumable_codex_exited_arbiter_dead_converges() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Working).await;
        let task = insert_task(&repo, &wave_id, "codex-dead", TaskStatus::Running).await;
        let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
        // `created_at_ms = 1` ⇒ `now - last` (NULL last_activity ⇒ created_at)
        // is far past the deadline, so the pre-gate does not short-circuit.
        let mut worker = session("ws-codex-dead", wave_id.clone(), 1);
        worker.provider = WorkerProviderKind::Codex;
        worker.mode = SessionMode::Resumable;
        worker.thread_id = Some("t-codex-dead".into());
        worker.spawn_op_id = Some(op_id);
        insert_session(&repo, worker).await;

        let fake = Arc::new(
            FakeProvider::new()
                .with_session_mode(SessionMode::Resumable)
                .with_death_verdict(DeathVerdict::Dead)
                .with_probe_script([exited_liveness()]),
        );
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry_for(WorkerProviderKind::Codex, fake.clone()),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_all().await;

        assert_eq!(fake.probe_call_count(), 1);
        assert_eq!(
            fake.death_verdict_call_count(),
            1,
            "arbiter must be consulted for a stale resumable Exited"
        );

        let worker = repo
            .session_get(&WorkerSessionId::from("ws-codex-dead"))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(worker.state, WorkerSessionState::Failed);
        assert_eq!(worker.liveness, LivenessTag::Exited);
        assert_eq!(worker.exit_code, Some(7));
        assert_eq!(worker.exit_interpretation.as_deref(), Some("failed"));
        assert!(worker.completed_at_ms.is_some());

        let task_row = repo
            .task_get(&task.id)
            .await
            .expect("task get")
            .expect("task exists");
        assert_eq!(task_row.status, TaskStatus::Failed);
        assert_eq!(task_row.status_detail.as_deref(), Some("spawn-failed"));

        let failed = task_failed_events(&repo, &task.id).await;
        assert_eq!(failed.len(), 1);

        let changes = lifecycle_changes(&repo, &wave_id).await;
        assert_eq!(changes.len(), 1);
        match &changes[0] {
            Event::WaveLifecycleChanged { from, to, .. } => {
                assert_eq!(*from, WaveLifecycle::Working);
                assert_eq!(*to, WaveLifecycle::Reviewing);
            }
            other => panic!("expected lifecycle change, got {other:?}"),
        }
        let wave = repo
            .wave_get(wave_id.as_str())
            .await
            .expect("wave get")
            .expect("wave exists");
        assert_eq!(wave.lifecycle, WaveLifecycle::Reviewing);

        reset_reaper_boot_gate_for_test();
    }

    #[tokio::test]
    async fn sweep_resumable_codex_dead_worker_releases_same_boot_workspace_lease() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Working).await;
        let task = insert_task(&repo, &wave_id, "codex-lease-dead", TaskStatus::Running).await;
        let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
        let mut worker = session("ws-codex-lease-dead", wave_id.clone(), 1);
        worker.provider = WorkerProviderKind::Codex;
        worker.mode = SessionMode::Resumable;
        worker.thread_id = Some("t-codex-lease-dead".into());
        worker.spawn_op_id = Some(op_id.clone());
        let card = insert_session(&repo, worker).await;
        let (lease_id, lease_path) =
            acquire_test_workspace_lease(&repo, card.id.as_str(), &wave_id, &op_id).await;
        assert!(
            std::path::Path::new(&lease_path).is_dir(),
            "leased cwd exists before reaping"
        );

        let fake = Arc::new(
            FakeProvider::new()
                .with_session_mode(SessionMode::Resumable)
                .with_death_verdict(DeathVerdict::Dead)
                .with_probe_script([exited_liveness()]),
        );
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry_for(WorkerProviderKind::Codex, fake),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_all().await;

        let state: String =
            sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
                .bind(&lease_id)
                .fetch_one(repo.pool())
                .await
                .expect("lease state");
        assert_eq!(state, "released");
        assert!(
            !std::path::Path::new(&lease_path).exists(),
            "reaper release removes leased cwd"
        );
        let released_events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
                .fetch_one(repo.pool())
                .await
                .expect("released event count");
        assert_eq!(released_events, 1);

        reset_reaper_boot_gate_for_test();
    }

    /// #741-3 (b): a resumable Exited whose arbiter returns `Alive` records a
    /// T2 liveness observation and does NOT converge.
    #[tokio::test]
    async fn sweep_resumable_codex_exited_arbiter_alive_records_t2_only() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Working).await;
        let task = insert_task(&repo, &wave_id, "codex-alive", TaskStatus::Running).await;
        let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
        let mut worker = session("ws-codex-alive", wave_id.clone(), 1);
        worker.provider = WorkerProviderKind::Codex;
        worker.mode = SessionMode::Resumable;
        worker.thread_id = Some("t-codex-alive".into());
        worker.spawn_op_id = Some(op_id);
        insert_session(&repo, worker).await;

        let fake = Arc::new(
            FakeProvider::new()
                .with_session_mode(SessionMode::Resumable)
                .with_death_verdict(DeathVerdict::Alive)
                .with_probe_script([exited_liveness()]),
        );
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry_for(WorkerProviderKind::Codex, fake.clone()),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_all().await;

        assert_eq!(fake.probe_call_count(), 1);
        assert_eq!(fake.death_verdict_call_count(), 1);

        let worker = repo
            .session_get(&WorkerSessionId::from("ws-codex-alive"))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(worker.liveness, LivenessTag::Exited);
        assert!(worker.liveness_probed_at_ms.is_some());
        assert_eq!(
            worker.state,
            WorkerSessionState::Running,
            "arbiter Alive must NOT terminalize the session"
        );
        assert_eq!(worker.exit_code, None);
        assert_eq!(worker.exit_interpretation, None);
        assert!(worker.completed_at_ms.is_none());

        assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
        assert_eq!(lifecycle_changes(&repo, &wave_id).await.len(), 0);
        let task_row = repo
            .task_get(&task.id)
            .await
            .expect("task get")
            .expect("task exists");
        assert_eq!(task_row.status, TaskStatus::Running);

        reset_reaper_boot_gate_for_test();
    }

    /// #741-3 (c): a resumable Exited whose arbiter returns `Unknown` records a
    /// T2 liveness observation and does NOT converge.
    #[tokio::test]
    async fn sweep_resumable_codex_exited_arbiter_unknown_records_t2_only() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Working).await;
        let task = insert_task(&repo, &wave_id, "codex-unknown", TaskStatus::Running).await;
        let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
        let mut worker = session("ws-codex-unknown", wave_id.clone(), 1);
        worker.provider = WorkerProviderKind::Codex;
        worker.mode = SessionMode::Resumable;
        worker.thread_id = Some("t-codex-unknown".into());
        worker.spawn_op_id = Some(op_id);
        insert_session(&repo, worker).await;

        let fake = Arc::new(
            FakeProvider::new()
                .with_session_mode(SessionMode::Resumable)
                .with_death_verdict(DeathVerdict::Unknown)
                .with_probe_script([exited_liveness()]),
        );
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry_for(WorkerProviderKind::Codex, fake.clone()),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_all().await;

        assert_eq!(fake.probe_call_count(), 1);
        assert_eq!(fake.death_verdict_call_count(), 1);

        let worker = repo
            .session_get(&WorkerSessionId::from("ws-codex-unknown"))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(worker.liveness, LivenessTag::Exited);
        assert!(worker.liveness_probed_at_ms.is_some());
        assert_eq!(
            worker.state,
            WorkerSessionState::Running,
            "arbiter Unknown must NOT terminalize the session"
        );
        assert_eq!(worker.exit_code, None);
        assert_eq!(worker.exit_interpretation, None);
        assert!(worker.completed_at_ms.is_none());

        assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
        assert_eq!(lifecycle_changes(&repo, &wave_id).await.len(), 0);

        reset_reaper_boot_gate_for_test();
    }

    /// #741-3 (d): a resumable Exited whose `last_activity_ms` is RECENT — the
    /// §1.1(d) pre-gate short-circuits to a T2 observation WITHOUT consulting
    /// the arbiter (no RPC). Arbiter would say `Dead`, but it is never asked.
    #[tokio::test]
    async fn sweep_resumable_codex_exited_recent_activity_pregate_skips_arbiter() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Working).await;
        let task = insert_task(&repo, &wave_id, "codex-recent", TaskStatus::Running).await;
        let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
        let mut worker = session("ws-codex-recent", wave_id.clone(), 1);
        worker.provider = WorkerProviderKind::Codex;
        worker.mode = SessionMode::Resumable;
        worker.thread_id = Some("t-codex-recent".into());
        // RECENT activity: well within the default 15-min deadline window.
        worker.last_activity_ms = Some(now_ms());
        worker.spawn_op_id = Some(op_id);
        insert_session(&repo, worker).await;

        let fake = Arc::new(
            FakeProvider::new()
                .with_session_mode(SessionMode::Resumable)
                // Arbiter WOULD reap, proving the pre-gate is what holds it.
                .with_death_verdict(DeathVerdict::Dead)
                .with_probe_script([exited_liveness()]),
        );
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry_for(WorkerProviderKind::Codex, fake.clone()),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_all().await;

        assert_eq!(fake.probe_call_count(), 1);
        assert_eq!(
            fake.death_verdict_call_count(),
            0,
            "recent-activity pre-gate must short-circuit WITHOUT consulting the arbiter"
        );

        let worker = repo
            .session_get(&WorkerSessionId::from("ws-codex-recent"))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(worker.liveness, LivenessTag::Exited);
        assert!(worker.liveness_probed_at_ms.is_some());
        assert_eq!(
            worker.state,
            WorkerSessionState::Running,
            "recent-activity pre-gate must NOT terminalize the session"
        );
        assert_eq!(worker.exit_code, None);
        assert_eq!(worker.exit_interpretation, None);

        assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
        assert_eq!(lifecycle_changes(&repo, &wave_id).await.len(), 0);

        reset_reaper_boot_gate_for_test();
    }

    /// P2 (spawn-window false-convergence): an EPHEMERAL (terminal) session
    /// still in the `starting` state observed `Exited` must NOT converge — a
    /// supervisor `proc_running:false` in the spawn window means "not
    /// registered YET", not "exited". The reaper records the liveness as a T2
    /// observation (`liveness` column set) and leaves the session in `starting`
    /// with no `TaskFailed` and no lifecycle change; the spawn operation owns
    /// convergence for `starting` sessions.
    #[tokio::test]
    async fn sweep_exited_starting_session_records_liveness_without_convergence() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Working).await;
        let task = insert_task(&repo, &wave_id, "spawn-window", TaskStatus::Running).await;
        let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
        let mut worker = session("ws-starting", wave_id.clone(), 1);
        // EPHEMERAL terminal worker still in the spawn/startup window: the
        // `worker_session` row exists before the PTY registers with the
        // proc-supervisor, so the probe's `proc_running:false` is "not spawned
        // YET", not "exited".
        worker.state = WorkerSessionState::Starting;
        worker.spawn_op_id = Some(op_id);
        insert_session(&repo, worker).await;

        let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry(fake.clone()),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_all().await;

        assert_eq!(fake.probe_call_count(), 1);
        let worker = repo
            .session_get(&WorkerSessionId::from("ws-starting"))
            .await
            .expect("session get")
            .expect("session exists");
        // T2 observation recorded: liveness column set, NOT terminalized.
        assert_eq!(worker.liveness, LivenessTag::Exited);
        assert!(worker.liveness_probed_at_ms.is_some());
        assert_eq!(
            worker.state,
            WorkerSessionState::Starting,
            "spawn-window Exited must NOT terminalize a `starting` session"
        );
        assert_eq!(worker.exit_code, None, "no exit committed in spawn window");
        assert_eq!(worker.exit_interpretation, None);
        assert!(worker.completed_at_ms.is_none());

        // No convergence: no task.failed, task stays running, wave stays Working.
        assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
        assert_eq!(lifecycle_changes(&repo, &wave_id).await.len(), 0);
        let task_row = repo
            .task_get(&task.id)
            .await
            .expect("task get")
            .expect("task exists");
        assert_eq!(task_row.status, TaskStatus::Running);
        let wave = repo
            .wave_get(wave_id.as_str())
            .await
            .expect("wave get")
            .expect("wave exists");
        assert_eq!(wave.lifecycle, WaveLifecycle::Working);

        reset_reaper_boot_gate_for_test();
    }

    #[tokio::test]
    async fn sweep_exited_with_null_spawn_op_task_key_terminalizes_without_task_failed() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Working).await;
        let task = insert_task(&repo, &wave_id, "null-op-key", TaskStatus::Running).await;
        let op_id = insert_spawn_operation(&repo, None, None).await;
        let mut worker = session("ws-null-op-key", wave_id.clone(), 1);
        worker.spawn_op_id = Some(op_id);
        insert_session(&repo, worker).await;

        let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry(fake),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_all().await;

        let worker = repo
            .session_get(&WorkerSessionId::from("ws-null-op-key"))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(worker.state, WorkerSessionState::Failed);
        assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
        assert_eq!(lifecycle_changes(&repo, &wave_id).await.len(), 0);
        let wave = repo
            .wave_get(wave_id.as_str())
            .await
            .expect("wave get")
            .expect("wave exists");
        assert_eq!(wave.lifecycle, WaveLifecycle::Working);

        reset_reaper_boot_gate_for_test();
    }

    #[tokio::test]
    async fn sweep_exited_race_lost_after_live_terminal_completion_emits_no_second_event() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Working).await;
        let task = insert_task(&repo, &wave_id, "race", TaskStatus::Running).await;
        let mut worker = session("ws-race", wave_id.clone(), 1);
        let worker_card = insert_session(&repo, worker.clone()).await;
        let op_id =
            insert_spawn_operation(&repo, Some(&task.id), Some(worker_card.id.as_str())).await;
        worker.spawn_op_id = Some(op_id);
        sqlx::query("UPDATE worker_sessions SET spawn_op_id = ?1 WHERE id = ?2")
            .bind(worker.spawn_op_id.as_deref())
            .bind(worker.id.as_str())
            .execute(repo.pool())
            .await
            .expect("stamp session spawn op");

        let events = EventBus::new();
        let write = write_context(&repo).await;
        crate::scheduler::complete_terminal_task(
            repo.as_ref(),
            &events,
            &write,
            &task.id,
            wave_id.as_str(),
            worker_card.id.as_str(),
            Some(0),
            false,
        )
        .await
        .expect("live terminal completion");

        let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(repo_dyn, registry(fake), events, write);

        reaper_on_boot();
        reaper.sweep_all().await;

        assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
        let task_row = repo
            .task_get(&task.id)
            .await
            .expect("task get")
            .expect("task exists");
        assert_eq!(task_row.status, TaskStatus::Done);
        let completed = RepoEventWrite::events_since(repo.as_ref(), 0, None)
            .await
            .expect("events")
            .into_iter()
            .filter(|(_id, _version, _scope, event)| {
                matches!(event, Event::TaskCompleted { idempotency_key, .. } if idempotency_key == &task.id)
            })
            .count();
        assert_eq!(completed, 1);
        let changes = lifecycle_changes(&repo, &wave_id).await;
        assert_eq!(changes.len(), 1);
        let worker = repo
            .session_get(&WorkerSessionId::from("ws-race"))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(worker.state, WorkerSessionState::Failed);

        reset_reaper_boot_gate_for_test();
    }

    #[tokio::test]
    async fn sweep_unknown_liveness_records_t2_without_death_convergence() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Working).await;
        let task = insert_task(&repo, &wave_id, "unknown", TaskStatus::Running).await;
        let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
        let mut worker = session("ws-unknown-death", wave_id.clone(), 1);
        worker.spawn_op_id = Some(op_id);
        insert_session(&repo, worker).await;

        let fake =
            Arc::new(FakeProvider::new().with_probe_script([Liveness::Unknown { since_ms: 55 }]));
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry(fake),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper_on_boot();
        reaper.sweep_all().await;

        let worker = repo
            .session_get(&WorkerSessionId::from("ws-unknown-death"))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(worker.state, WorkerSessionState::Running);
        assert_eq!(worker.liveness, LivenessTag::Unknown);
        assert!(worker.liveness_probed_at_ms.is_some());
        let task_row = repo
            .task_get(&task.id)
            .await
            .expect("task get")
            .expect("task exists");
        assert_eq!(task_row.status, TaskStatus::Running);
        assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
        assert_eq!(lifecycle_changes(&repo, &wave_id).await.len(), 0);

        reset_reaper_boot_gate_for_test();
    }

    #[tokio::test]
    async fn sweep_noops_until_reaper_on_boot_opens_gate() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        insert_session(&repo, session("ws-gated", wave_id, 1)).await;
        let fake = Arc::new(FakeProvider::new().with_probe_script([Liveness::Alive {
            active_turn_id: None,
        }]));
        let repo_dyn: Arc<dyn Repo> = repo.clone();
        let reaper = Reaper::new(
            repo_dyn,
            registry(fake.clone()),
            EventBus::new(),
            write_context(&repo).await,
        );

        reaper.sweep_all().await;
        assert_eq!(fake.probe_call_count(), 0);
        let before = repo
            .session_get(&WorkerSessionId::from("ws-gated"))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(before.liveness, LivenessTag::Unknown);
        assert_eq!(before.liveness_probed_at_ms, None);

        reaper_on_boot();
        reaper.sweep_all().await;

        assert_eq!(fake.probe_call_count(), 1);
        let after = repo
            .session_get(&WorkerSessionId::from("ws-gated"))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(after.liveness, LivenessTag::Alive);
        assert_eq!(after.state, WorkerSessionState::Running);
        assert!(after.liveness_probed_at_ms.is_some());

        reset_reaper_boot_gate_for_test();
    }
}
