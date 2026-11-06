use super::*;
use calm_server::event::EventBus;
use calm_server::model::{TaskStatus, now_ms};
use calm_server::scheduler::TerminalTaskHook;
use calm_server::session_projection_repo::{
    WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::state::WriteContext;
use calm_truth::db::sqlite::session_start_runtime_tx;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    None,
    Terminal,
    Session,
    Task,
}

#[tokio::test]
async fn fd_resources_terminal_write_failure_keeps_writer_until_teardown() {
    exit_persistence(Failure::Terminal).await;
}

#[tokio::test]
async fn fd_resources_session_write_failure_keeps_writer_until_teardown() {
    exit_persistence(Failure::Session).await;
}

#[tokio::test]
async fn fd_resources_task_write_failure_keeps_writer_until_teardown() {
    exit_persistence(Failure::Task).await;
}

#[tokio::test]
async fn fd_resources_persisted_exit_releases_writer_with_repo() {
    exit_persistence(Failure::None).await;
}

async fn seed_exit_callbacks(repo: &SqlxRepo, term: &Terminal) -> (String, String) {
    let card = repo.card_get(term.card_id.as_str()).await.unwrap().unwrap();
    let task_id = format!("{}:exit-fence", card.track_id);
    let session_id = Uuid::new_v4().to_string();
    repo.db()
        .writer()
        .with_writer(|writer| {
            let task_id = task_id.clone();
            let session_id = session_id.clone();
            let terminal_id = term.id.clone();
            Box::pin(async move {
                let mut tx = writer.begin_immediate().await.unwrap();
                session_start_runtime_tx(
                    &mut tx,
                    WorkerSessionInit {
                        id: session_id,
                        card_id: card.id.to_string(),
                        kind: WorkerSessionKind::Terminal,
                        agent_provider: None,
                        status: WorkerSessionState::Running,
                        terminal_run_id: Some(terminal_id),
                        thread_id: None,
                        session_id: None,
                        active_turn_id: None,
                        handle_state_json: None,
                        spawn_op_id: None,
                        now_ms: now_ms(),
                    },
                )
                .await
                .unwrap();
                sqlx::query("UPDATE cards SET payload=json_set(payload,'$.idempotency_key',?1) WHERE id=?2")
                    .bind(&task_id)
                    .bind(card.id.as_str())
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,created_at_ms,updated_at_ms) \
                    VALUES(?1,?2,'exit-fence','terminal','true','null','running',?3,?4,?4)")
                    .bind(&task_id)
                    .bind(card.track_id.as_str())
                    .bind(card.id.as_str())
                    .bind(now_ms())
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
    (session_id, task_id)
}

fn task_hook(repo: Arc<SqlxRepo>) -> Arc<TerminalTaskHook> {
    TerminalTaskHook::new(
        repo,
        EventBus::new(),
        WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
    )
}

async fn exit_persistence(failure: Failure) {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("supervisor.sock");
    let release = temp.path().join("release");
    let mut supervisor = spawn_proc_supervisor(&socket).await;
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let term = seed_terminal_row(&repo).await;
    let registry = TerminalRendererRegistry::new_with_repo(repo.clone());
    registry.set_task_hook(task_hook(repo.clone()));
    let entry = registry
        .ensure(super::fd_resources::config(
            &socket,
            &term.id,
            &format!(
                "while [ ! -e '{}' ]; do sleep 0.02; done; exit 0",
                release.display()
            ),
        ))
        .await
        .unwrap();
    let (session_id, task_id) = seed_exit_callbacks(&repo, &term).await;
    // Install after ensure: only the intended exit write fails, never startup.
    let trigger = match failure {
        Failure::None => None,
        Failure::Terminal => Some(format!(
            "CREATE TRIGGER reject_exit BEFORE UPDATE OF exit_code ON terminals \
             WHEN NEW.id='{}' BEGIN SELECT RAISE(ABORT,'reject terminal exit'); END",
            term.id
        )),
        Failure::Session => Some(format!(
            "CREATE TRIGGER reject_exit BEFORE UPDATE OF state ON worker_sessions \
             WHEN NEW.id='{session_id}' AND NEW.state='exited' \
             BEGIN SELECT RAISE(ABORT,'reject session exit'); END"
        )),
        Failure::Task => Some(format!(
            "CREATE TRIGGER reject_exit BEFORE UPDATE OF status ON tasks \
             WHEN NEW.id='{task_id}' AND NEW.status='done' \
             BEGIN SELECT RAISE(ABORT,'reject task exit'); END"
        )),
    };
    if let Some(trigger) = trigger {
        repo.db()
            .writer()
            .execute(sqlx::query(&trigger))
            .await
            .unwrap();
    }
    let (wake_tx, mut wake_rx) = mpsc::unbounded_channel();
    registry.set_output_wake(wake_tx);
    let outcome = async {
        std::fs::write(&release, b"")?;
        anyhow::ensure!(timeout(LIVENESS_BUDGET, entry.wait_exited()).await? == Some(Some(0)));
        // The fixture emits no PTY bytes. This wake is AFTER the real terminal write,
        // session completion and task hook returned, including their error branches.
        anyhow::ensure!(timeout(LIVENESS_BUDGET, wake_rx.recv()).await? == Some(term.id.clone()));
        anyhow::ensure!(
            entry
                .last_output_ms
                .load(std::sync::atomic::Ordering::Relaxed)
                == 0
        );
        let persisted = timeout(LIVENESS_BUDGET, entry.wait_exit_reader_closed_for_test()).await?;
        anyhow::ensure!(entry.wait_exit_persisted_for_test(LIVENESS_BUDGET).await == persisted);
        let terminal = repo.terminal_get(&term.id).await?.unwrap();
        anyhow::ensure!(
            terminal.exit_code
                == if failure == Failure::Terminal {
                    None
                } else {
                    Some(0)
                }
        );
        let session = repo.session_projection_by_id(&session_id).await?.unwrap();
        anyhow::ensure!(
            session.status
                == if failure == Failure::Session {
                    WorkerSessionState::Running
                } else {
                    WorkerSessionState::Exited
                }
        );
        let task = repo.task_get(&task_id).await?.unwrap();
        anyhow::ensure!(
            task.status
                == if failure == Failure::Task {
                    TaskStatus::Running
                } else {
                    TaskStatus::Done
                }
        );
        if failure == Failure::None {
            anyhow::ensure!(persisted, "successful exit must notify persistence");
            timeout(LIVENESS_BUDGET, entry.handle.supervisor_tx.closed()).await?;
        } else {
            // Yield to the real writer's completion branch too; the primary assertion
            // above is synchronized by the callback wake, not a negative timeout.
            let closed = timeout(
                Duration::from_millis(100),
                entry.handle.supervisor_tx.closed(),
            )
            .await;
            anyhow::ensure!(
                !persisted,
                "failed exit write falsely notified persistence; writer_closed={}",
                closed.is_ok()
            );
            anyhow::ensure!(
                closed.is_err(),
                "failed exit write released the control writer"
            );
        }
        anyhow::ensure!(entry.observable() && registry.get(&term.id).is_some());
        Ok::<_, anyhow::Error>(())
    }
    .await;
    // Clean up on red runs too; teardown keeps TERM grace on ReaderGone, then KILL
    // and cancellation close the original writer without inventing persistence.
    let started = std::time::Instant::now();
    registry.drop_entry(&term.id).await;
    let elapsed = started.elapsed();
    timeout(LIVENESS_BUDGET, entry.handle.supervisor_tx.closed())
        .await
        .unwrap();
    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
    assert!(outcome.is_ok(), "{outcome:?}");
    if failure != Failure::None {
        assert!(
            elapsed >= TERM_TO_KILL_GRACE,
            "failed exit shortened TERM grace: {elapsed:?}"
        );
    }
}

#[tokio::test]
async fn fd_resources_accepted_stop_finishes_after_exit_write_failure() {
    use super::fd_resources::{config, member_alive};
    use calm_server::terminal_renderer::SupervisorControl;
    use calm_session::control::ProcSignal;

    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("supervisor.sock");
    let pid_file = temp.path().join("member.pid");
    let mut supervisor = spawn_proc_supervisor(&socket).await;
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let term = seed_terminal_row(&repo).await;
    let registry = TerminalRendererRegistry::new_with_repo(repo.clone());
    registry.set_task_hook(task_hook(repo.clone()));
    let entry = registry.ensure(config(&socket, &term.id, &format!(
        "trap 'exit 0' HUP; sh -c 'trap \"\" HUP TERM; echo $$ > \"$1\"; exec sleep 600' sh '{}' & while :; do sleep 1; done",
        pid_file.display()
    ))).await.unwrap();
    let (session_id, task_id) = seed_exit_callbacks(&repo, &term).await;
    repo.db()
        .writer()
        .execute(sqlx::query(&format!(
            "CREATE TRIGGER reject_exit BEFORE UPDATE OF exit_code ON terminals \
         WHEN NEW.id='{}' BEGIN SELECT RAISE(ABORT,'reject terminal exit'); END",
            term.id
        )))
        .await
        .unwrap();
    let outcome = async {
        let pid = timeout(LIVENESS_BUDGET, wait_for_pid_file(&pid_file)).await?;
        anyhow::ensure!(member_alive(pid).await);
        entry.handle.supervisor_tx.send(SupervisorControl::Stop)?;
        anyhow::ensure!(timeout(LIVENESS_BUDGET, entry.wait_exited()).await? == Some(Some(0)));
        let persisted = timeout(LIVENESS_BUDGET, entry.wait_exit_reader_closed_for_test()).await?;
        anyhow::ensure!(
            member_alive(pid).await,
            "leader exit must precede member death"
        );
        anyhow::ensure!(
            repo.terminal_get(&term.id)
                .await?
                .unwrap()
                .exit_code
                .is_none()
        );
        anyhow::ensure!(
            repo.session_projection_by_id(&session_id)
                .await?
                .unwrap()
                .status
                == WorkerSessionState::Exited
        );
        anyhow::ensure!(repo.task_get(&task_id).await?.unwrap().status == TaskStatus::Done);
        // Stop was accepted before the failure. Its original writer must still deliver
        // the delayed KILL even though the reader dropped its unsuccessful exit sender.
        timeout(Duration::from_secs(5), async {
            while member_alive(pid).await {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        let closed = timeout(
            Duration::from_millis(100),
            entry.handle.supervisor_tx.closed(),
        )
        .await;
        anyhow::ensure!(
            !persisted,
            "failed exit write falsely notified persistence; writer_closed={}",
            closed.is_ok()
        );
        anyhow::ensure!(
            closed.is_err(),
            "failed exit released writer after accepted Stop"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    entry.shutdown_signal(ProcSignal::Kill).await;
    let started = std::time::Instant::now();
    registry.drop_entry(&term.id).await;
    let elapsed = started.elapsed();
    timeout(LIVENESS_BUDGET, entry.handle.supervisor_tx.closed())
        .await
        .unwrap();
    let _ = supervisor.kill().await;
    let _ = supervisor.wait().await;
    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(elapsed >= TERM_TO_KILL_GRACE);
}
