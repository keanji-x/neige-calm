use super::*;
use crate::db::sqlite::SqlxRepo;
use crate::model::{Card, TaskAccess};
use crate::operation::{OperationCompletionBus, OperationRepo, SpawnCtx, SqlxOperationRepo};
use calm_truth::db::{RepoRead, RepoSyncDomainRaw};
use calm_truth::session_repo::SessionRepo;
use calm_types::worker::{WorkerSessionId, WorkerSessionState};

struct Fixture {
    repo: Arc<SqlxRepo>,
    scheduler: Arc<Scheduler>,
    _runtime: Arc<OperationRuntime>,
    _cwd: tempfile::TempDir,
    card: Card,
    task: Task,
    lease: String,
}

impl Fixture {
    async fn new(access: TaskAccess) -> Self {
        let cwd = crate::operation::workspace_lease::upstream_tests::attached_repo();
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let area = repo
            .area_create(
                calm_types::organization::LEGACY_OWNER_USER_ID,
                crate::model::NewArea {
                    name: "timeout fence".into(),
                    color: "#000".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = crate::test_seams::create_track_with_workspace_for_test(
            repo.db(),
            crate::model::NewTrack {
                template_input: None,
                area_id: area.id,
                title: "timeout fence".into(),
                sort: None,
                cwd: cwd.path().to_str().unwrap().into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            },
            &crate::db::sqlite::TrackWorkspacePlan::AttachedWithTrackWorktree(cwd.path().into()),
            repo.track_area_cache(),
        )
        .await
        .unwrap();
        crate::test_support::ensure_track_worktree(repo.db(), track.id.as_str()).await;
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let (card, _) = calm_truth::db::sqlite::card_with_claude_worker_create_tx(
            &mut tx,
            "card-timeout".into(),
            "session-a",
            None,
            track.id.clone(),
            None,
            None,
            "worker".into(),
            cwd.path().to_str().unwrap().into(),
            json!({}),
            Some("do".into()),
            None,
            None,
            cwd.path().join("settings.json").to_str().unwrap().into(),
            "agent-a".into(),
            repo.card_role_cache(),
            crate::routes::theme::RequestTheme::default_dark(),
        )
        .await
        .unwrap();
        let mut task = tests::task("a", TaskStatus::Running, &[], 0);
        task.id = format!("{}:a", track.id);
        task.track_id = track.id.to_string();
        task.kind = TaskKind::Claude;
        task.access = access;
        task.worker_card_id = Some(card.id.to_string());
        crate::test_support::insert_task_tx(&mut tx, &task)
            .await
            .unwrap();
        sqlx::query("UPDATE worker_sessions SET state = 'running' WHERE id = 'session-a'")
            .execute(&mut **tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let operations = Arc::new(SqlxOperationRepo::new(repo.db().clone()));
        let op = operations
            .insert_operation(
                "claude-worker",
                OperationKey {
                    operation_key: "spawn-a".into(),
                    idempotency_key: Some(task.id.clone()),
                    payload_hash: "a".into(),
                },
                json!({}),
            )
            .await
            .unwrap();
        sqlx::query("UPDATE worker_sessions SET spawn_op_id = ? WHERE id = 'session-a'")
            .bind(&op)
            .execute(repo.pool())
            .await
            .unwrap();
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let plan = crate::operation::workspace_lease::prepare_worker_lease_as_tx(
            &mut tx,
            &task.track_id,
            access,
            cwd.path(),
        )
        .await
        .unwrap();
        let (lease, _) = crate::operation::workspace_lease::acquire_workspace_lease_tx(
            &mut tx,
            card.id.as_str(),
            &task.track_id,
            &op,
            &plan,
        )
        .await
        .unwrap();
        // The production timeout entry point marks the live session after ending A.
        sqlx::query(
            "UPDATE tasks SET status = 'failed', status_detail = 'worker-timeout' WHERE id = ?",
        )
        .bind(&task.id)
        .execute(&mut **tx)
        .await
        .unwrap();
        let mark = mark_running_timeout_cleanup_tx(
            &mut tx,
            card.id.as_str(),
            &task.id,
            now_ms(),
            WorkerCleanupReason::LivenessTimeout,
        )
        .await
        .unwrap();
        assert_eq!(mark.marked, 1);
        tx.commit().await.unwrap();
        let events = EventBus::new();
        let completion = OperationCompletionBus::new();
        let runtime = Arc::new(OperationRuntime::new_unchecked(
            operations.clone(),
            Vec::new(),
            events.clone(),
            completion.clone(),
            SpawnCtx::new(
                repo.clone(),
                operations,
                Arc::new(crate::state::DaemonClient::new_stub()),
                crate::terminal_renderer::TerminalRendererRegistry::new_with_repo(repo.clone()),
                events.clone(),
                completion,
            ),
        ));
        let scheduler = Scheduler::new(
            repo.clone(),
            events,
            WriteContext::new(
                repo.card_role_cache().clone(),
                repo.track_area_cache().clone(),
            ),
            Arc::downgrade(&runtime),
            crate::per_card_lock::new_per_card_locks(),
            Arc::new(Semaphore::new(1)),
            cwd.path().join("gate-logs"),
            WorkerLiveness::DEFAULT,
            WorkerIdleWake::new(
                crate::shared_codex_appserver::SharedCodexAppServer::new_stub(repo.clone()),
                WORKER_IDLE_TURN_GRACE,
                WORKER_IDLE_PROBE_TIMEOUT,
            ),
        );
        Self {
            repo,
            scheduler,
            _runtime: runtime,
            _cwd: cwd,
            card,
            task,
            lease: lease.lease_id,
        }
    }

    async fn lease_state(&self) -> String {
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?")
            .bind(&self.lease)
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    async fn marker(&self) -> Option<String> {
        sqlx::query_scalar("SELECT json_extract(handle_state_json, '$.timeout_cleanup') FROM worker_sessions WHERE id = 'session-a'")
            .fetch_one(self.repo.pool()).await.unwrap()
    }
}

// A pipe keeps the worker alive without a timer or a scheduler-dependent sleep.
async fn attach_worker(f: &Fixture, session_id: &str) -> tokio::process::Child {
    let child = tokio::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let terminal: String = sqlx::query_scalar("SELECT id FROM terminals WHERE card_id = ?")
        .bind(f.card.id.as_str())
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE terminals SET pid = ? WHERE id = ?")
        .bind(i64::from(child.id().unwrap()))
        .bind(&terminal)
        .execute(f.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET terminal_run_id = ? WHERE id = ?")
        .bind(&terminal)
        .bind(session_id)
        .execute(f.repo.pool())
        .await
        .unwrap();
    child
}

// B may itself have ended while its worker/lease cleanup is still pending. Exactly(A) must
// protect that lease too; checking only that the latest lease's task ended is insufficient.
#[tokio::test]
async fn w_timeout_cleanup_spares_successor_attempt() {
    for access in [TaskAccess::ReadOnly, TaskAccess::ReadWrite] {
        for same_session in [false, true] {
            for status in [TaskStatus::Running, TaskStatus::Failed] {
                let mut f = Fixture::new(access).await;
                let hook = ClaimFenceTestHook {
                    resolved: Arc::new(Notify::new()),
                    resume: Arc::new(Notify::new()),
                };
                *f.scheduler
                    .timeout_cleanup_discovered_test_hook
                    .lock()
                    .unwrap() = Some(hook.clone());
                let scheduler = f.scheduler.clone();
                let sweep =
                    tokio::spawn(async move { scheduler.sweep_timeout_worker_cleanups().await });
                hook.resolved.notified().await;
                let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
                crate::operation::workspace_lease::release_workspace_lease_for_card_tx(
                    &mut tx,
                    f.card.id.as_str(),
                    ReleaseDelivery::CommitAsTaskEnded,
                )
                .await
                .unwrap();
                let mut b = f.task.clone();
                b.id = format!("{}:b", b.track_id);
                b.key = "b".into();
                b.status = status;
                crate::test_support::insert_task_tx(&mut tx, &b)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                let operations = SqlxOperationRepo::new(f.repo.db().clone());
                let op = operations
                    .insert_operation(
                        "claude-worker",
                        OperationKey {
                            operation_key: "spawn-b".into(),
                            idempotency_key: Some(b.id.clone()),
                            payload_hash: "b".into(),
                        },
                        json!({}),
                    )
                    .await
                    .unwrap();
                let mut tx = begin_immediate_tx(f.repo.db()).await.unwrap();
                if same_session {
                    // A stale marker on a current carrier must not authorize B's spawn op.
                    sqlx::query(
                        "UPDATE worker_sessions SET spawn_op_id = ? WHERE id = 'session-a'",
                    )
                    .bind(&op)
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                } else {
                    sqlx::query(
                        "UPDATE worker_sessions SET state = 'exited' WHERE id = 'session-a'",
                    )
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                    let mut session = calm_truth::db::sqlite::session_get_tx(
                        &mut tx,
                        &WorkerSessionId::from("session-a"),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    session.id = WorkerSessionId::from("session-b");
                    session.state = WorkerSessionState::Running;
                    session.spawn_op_id = Some(op.clone());
                    session.handle_state_json = None;
                    session.terminal_run_id = None;
                    calm_truth::db::sqlite::session_insert_tx(&mut tx, session)
                        .await
                        .unwrap();
                    sqlx::query("UPDATE cards SET session_id = 'session-b' WHERE id = ?")
                        .bind(f.card.id.as_str())
                        .execute(&mut **tx)
                        .await
                        .unwrap();
                }
                let plan = crate::operation::workspace_lease::prepare_worker_lease_as_tx(
                    &mut tx,
                    &b.track_id,
                    access,
                    f._cwd.path(),
                )
                .await
                .unwrap();
                let (lease, _) = crate::operation::workspace_lease::acquire_workspace_lease_tx(
                    &mut tx,
                    f.card.id.as_str(),
                    &b.track_id,
                    &op,
                    &plan,
                )
                .await
                .unwrap();
                f.lease = lease.lease_id;
                tx.commit().await.unwrap();
                let session_id = WorkerSessionId::from(if same_session {
                    "session-a"
                } else {
                    "session-b"
                });
                let mut worker = attach_worker(&f, session_id.as_str()).await;
                let before = f.repo.session_get(&session_id).await.unwrap().unwrap();
                let task_before = f.repo.task_get(&b.id).await.unwrap();
                hook.resume.notify_one();
                sweep.await.unwrap();
                let after = f.repo.session_get(&session_id).await.unwrap().unwrap();
                assert!(
                    worker.try_wait().unwrap().is_none(),
                    "B worker must stay alive"
                );
                worker.kill().await.unwrap();
                assert_eq!(
                    after.state, before.state,
                    "B session: {access:?}, same_session={same_session}, {status:?}"
                );
                assert_eq!(
                    f.lease_state().await,
                    "held",
                    "B lease: {access:?}, same_session={same_session}, {status:?}"
                );
                assert_eq!(f.repo.task_get(&b.id).await.unwrap(), task_before);
            }
        }
    }
}

#[tokio::test]
async fn w_timeout_cleanup_after_legacy_restart_reaps_same_attempt() {
    for access in [TaskAccess::ReadOnly, TaskAccess::ReadWrite] {
        let f = Fixture::new(access).await;
        // Legacy restart's S2 is current and has no native spawn-op binding.
        sqlx::query("UPDATE worker_sessions SET spawn_op_id = NULL WHERE id = 'session-a'")
            .execute(f.repo.pool())
            .await
            .unwrap();
        let mut worker = attach_worker(&f, "session-a").await;
        assert!(f.marker().await.is_some());
        let task_before = f.repo.task_get(&f.task.id).await.unwrap();
        f.scheduler.sweep_timeout_worker_cleanups().await;
        assert!(
            !worker.wait().await.unwrap().success(),
            "the same attempt worker must stop"
        );
        assert_eq!(f.lease_state().await, "released");
        assert!(f.marker().await.is_none());
        assert_eq!(f.repo.task_get(&f.task.id).await.unwrap(), task_before);
    }
}
