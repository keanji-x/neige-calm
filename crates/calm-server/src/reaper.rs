use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calm_exec::SpawnCtx;
use calm_types::worker::Liveness;

use crate::db::prelude::*;
use crate::model::now_ms;
use crate::provider_registry::WorkerProviderRegistry;

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
}

impl Reaper {
    pub fn new(repo: Arc<dyn Repo>, providers: WorkerProviderRegistry) -> Self {
        Self { repo, providers }
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

            if let Liveness::Exited { evidence } = &liveness {
                tracing::info!(
                    session_id = %session.id,
                    provider = session.provider.as_db_str(),
                    exit_code = ?evidence.exit_code,
                    signal_killed = evidence.signal_killed,
                    "reaper: observed exited liveness; session state unchanged in probe-only slice"
                );
            }

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

#[cfg(test)]
pub(crate) fn reset_reaper_boot_gate_for_test() {
    REAPER_BOOT_DONE.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    use calm_exec::WorkerProvider;
    use calm_truth::db::RepoEventWrite;
    use calm_truth::db::RepoSyncDomainRaw;
    use calm_truth::db::sqlite::{SqlxRepo, begin_immediate_tx, session_insert_tx};
    use calm_truth::session_repo::SessionRepo;
    use calm_truth_test_harness::FakeProvider;
    use calm_types::ids::WaveId;
    use calm_types::worker::{
        ExitEvidence, ExitSource, LivenessTag, SessionMode, WorkerContract, WorkerProviderKind,
        WorkerSession, WorkerSessionId, WorkerSessionState,
    };

    use crate::model::{NewCove, NewWave, RequestTheme};

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
            created_at_ms,
            updated_at_ms: created_at_ms,
            completed_at_ms: None,
        }
    }

    async fn insert_session(repo: &SqlxRepo, session: WorkerSession) {
        let mut tx = begin_immediate_tx(repo.pool()).await.expect("begin tx");
        session_insert_tx(&mut tx, session)
            .await
            .expect("insert session");
        tx.commit().await.expect("commit tx");
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
        WorkerProviderRegistry::from_entries([(
            WorkerProviderKind::Terminal,
            fake as Arc<dyn WorkerProvider>,
        )])
    }

    #[tokio::test]
    async fn sweep_records_liveness_without_events_or_state_transitions() {
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
        let reaper = Reaper::new(repo_dyn, registry(fake.clone()));
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
            "probe-only reaper must not emit events"
        );

        for (id, tag) in [
            ("ws-alive", LivenessTag::Alive),
            ("ws-idle", LivenessTag::Idle),
            ("ws-unknown", LivenessTag::Unknown),
            ("ws-exited", LivenessTag::Exited),
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
        let reaper = Reaper::new(repo_dyn, registry(fake.clone()));

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
