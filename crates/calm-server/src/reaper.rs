use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calm_exec::SpawnCtx;
use calm_types::worker::{
    ExitInterpretation, Liveness, SessionMode, WorkerSession, WorkerSessionState,
};

use crate::db::prelude::*;
use crate::db::sqlite::{TaskReporter, task_fail_from_worker_tx};
use crate::db::write_with_actor_events_typed;
use crate::error::Result;
use crate::event::{Event, EventBus, EventScope};
use crate::ids::ActorId;
use crate::model::WaveLifecycle;
use crate::model::now_ms;
use crate::provider_registry::WorkerProviderRegistry;
use crate::scheduler::{is_race_lost, race_lost_err};
use crate::state::WriteContext;
use crate::wave_lifecycle::auto_transition_if_current_in_tx;

pub const DEFAULT_REAPER_RECONCILE_SECS: u64 = 30;

static REAPER_BOOT_DONE: AtomicBool = AtomicBool::new(false);

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
}

impl Reaper {
    pub fn new(
        repo: Arc<dyn Repo>,
        providers: WorkerProviderRegistry,
        events: EventBus,
        write: WriteContext,
    ) -> Self {
        Self {
            repo,
            providers,
            events,
            write,
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
                    // FIX 1 (A1): 8b-ii converges EPHEMERAL workers only. A
                    // resumable provider (codex) whose PTY tore down does NOT
                    // mean the codex thread died — e.g. a proc-supervisor
                    // restart empties the registry while the codex thread
                    // survives on the separate daemon — so converging on a
                    // PTY `Exited` would FALSE-KILL live work. Record the
                    // probe as a T2 liveness observation (no terminalize, no
                    // event) and defer to the durable-codex-liveness design.
                    if provider.session_mode() != SessionMode::Ephemeral {
                        tracing::debug!(
                            session_id = %session.id,
                            provider = session.provider.as_db_str(),
                            "reaper: resumable provider Exited; convergence deferred to durable-codex-liveness design"
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
                                "reaper: failed to persist resumable Exited liveness observation"
                            );
                        }
                        continue;
                    }

                    // P2 (spawn-window false-convergence): `sessions_nonterminal`
                    // includes `starting`. A `starting` row exists BEFORE the
                    // spawn registers a PTY with the proc-supervisor, so a
                    // supervisor `ProbeOk{proc_running:false}` here means
                    // "not spawned/registered YET", NOT "exited". A slow/stuck
                    // spawn outliving a reaper tick would otherwise be FALSELY
                    // converged as a dead worker while the spawn operation is
                    // still responsible for completing/failing it. The spawn
                    // saga owns `starting` sessions — only converge a session
                    // that has actually STARTED. Record the probe as a T2
                    // liveness observation (no terminalize, no event) and let
                    // the spawn operation own convergence.
                    if !matches!(
                        session.state,
                        WorkerSessionState::Running
                            | WorkerSessionState::Idle
                            | WorkerSessionState::TurnPending
                    ) {
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
        Ok(_) => Ok(()),
        Err(e) if is_race_lost(&e) => Ok(()),
        Err(e) => Err(e),
    }
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
    use calm_types::ids::WaveId;
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

    async fn insert_session(repo: &SqlxRepo, session: WorkerSession) -> Card {
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
        // Mirror the session's state into the runtime row so the dual-write
        // parity-drop check (runtimes.status == worker_sessions.state) holds
        // for seeds in any state (e.g. `starting`), not just `running`.
        sqlx::query(
            r#"INSERT INTO runtimes (
                   id, card_id, kind, agent_provider, status, terminal_run_id,
                   thread_id, session_id, active_turn_id, handle_state_json,
                   lease_owner, lease_until_ms, created_at_ms, updated_at_ms,
                   completed_at_ms
               )
               VALUES (?1, ?2, 'terminal', NULL, ?5, ?3, NULL, NULL, NULL, NULL,
                       NULL, NULL, ?4, ?4, NULL)"#,
        )
        .bind(session.id.as_str())
        .bind(card.id.as_str())
        .bind(&session.terminal_run_id)
        .bind(session.created_at_ms)
        .bind(session.state.as_db_str())
        .execute(&mut *tx)
        .await
        .expect("insert runtime row");
        session_insert_tx(&mut tx, session)
            .await
            .expect("insert session");
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

    /// FIX 1 (A1): a CODEX (`SessionMode::Resumable`) session observed
    /// `Exited` must NOT converge — a torn-down PTY does not mean the codex
    /// thread died. The reaper records the liveness as a T2 observation
    /// (`liveness` column set) and leaves the session non-terminal with no
    /// `TaskFailed` and no lifecycle change.
    #[tokio::test]
    async fn sweep_resumable_codex_exited_records_liveness_without_convergence() {
        let _guard = REAPER_TEST_LOCK.lock().await;
        reset_reaper_boot_gate_for_test();

        let (repo, wave_id) = seeded_repo().await;
        set_wave_lifecycle(&repo, &wave_id, WaveLifecycle::Working).await;
        let task = insert_task(&repo, &wave_id, "codex-resumable", TaskStatus::Running).await;
        let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
        let mut worker = session("ws-codex-resumable", wave_id.clone(), 1);
        // A real codex session: resumable + codex provider.
        worker.provider = WorkerProviderKind::Codex;
        worker.mode = SessionMode::Resumable;
        worker.spawn_op_id = Some(op_id);
        insert_session(&repo, worker).await;

        // Resumable fake registered under the Codex kind so the reaper looks
        // it up and observes its `session_mode() == Resumable`.
        let fake = Arc::new(
            FakeProvider::new()
                .with_session_mode(SessionMode::Resumable)
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
        let worker = repo
            .session_get(&WorkerSessionId::from("ws-codex-resumable"))
            .await
            .expect("session get")
            .expect("session exists");
        // T2 observation recorded: liveness column set, NOT terminalized.
        assert_eq!(worker.liveness, LivenessTag::Exited);
        assert!(worker.liveness_probed_at_ms.is_some());
        assert_eq!(
            worker.state,
            WorkerSessionState::Running,
            "resumable codex Exited must NOT terminalize the session"
        );
        assert_eq!(worker.exit_code, None, "no exit committed for codex");
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
