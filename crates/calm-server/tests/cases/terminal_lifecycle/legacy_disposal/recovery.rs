use super::*;
use calm_session::control::{ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};

async fn poll(mut ready: impl FnMut() -> bool) {
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            tokio::time::Instant::now() < until,
            "condition did not converge"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
fn alive(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| s.rsplit_once(") ").map(|(_, tail)| !tail.starts_with('Z')))
        .unwrap_or(false)
}
async fn exited_group(present: bool, fault: bool) {
    let owner = Owner::new();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let proxy = fault.then(|| super::signal_fault::SignalFault::new(&owner));
    let sock = proxy.as_ref().map_or(&owner.sock, |proxy| &proxy.sock);
    let state = state_with_daemon(
        repo,
        DaemonClient {
            data_dir: owner._dir.path().into(),
            proc_supervisor_sock: Some(sock.clone()),
        },
    );
    let (_, _, card, term) = seed(&state).await;
    let pid_file = owner._dir.path().join("member");
    let ready = owner._dir.path().join("ready");
    let release = owner._dir.path().join("release");
    let script = format!(
        "(trap '' HUP TERM; : > '{}'; exec sleep 300) & echo $! > '{}'; \
         while [ ! -e '{}' ] || [ ! -e '{}' ]; do sleep 0.01; done; exit 0",
        ready.display(),
        pid_file.display(),
        ready.display(),
        release.display()
    );
    let cfg = calm_server::terminal_renderer::RendererConfig {
        terminal_id: term.clone(),
        cols: 80,
        rows: 24,
        buffer_bytes: 8192,
        terminal_fg: (220, 220, 220),
        terminal_bg: (15, 20, 24),
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script],
        envs: vec![],
        cwd: "/tmp".into(),
        supervisor_sock: sock.clone(),
    };
    let (entry, leader) = if present {
        let entry = state.terminal_renderer.ensure(cfg).await.unwrap();
        let leader = state
            .repo
            .terminal_get(&term)
            .await
            .unwrap()
            .unwrap()
            .pid
            .unwrap();
        (Some(entry), leader as u32)
    } else {
        let mut conn = calm_session::control::execution::connect_legacy(&owner.sock)
            .await
            .unwrap();
        write_frame(
            &mut conn,
            &ControlMsg::EnsureProc(EnsureProcRequest {
                proc_id: format!("term:{term}"),
                program: cfg.program,
                args: cfg.args,
                envs: cfg.envs,
                cwd: cfg.cwd,
                ready_timeout_ms: 0,
                io_mode: IoMode::Pty { cols: 80, rows: 24 },
                replay_bytes: 8192,
            }),
        )
        .await
        .unwrap();
        let ControlReply::Spawned { pid } = read_frame::<ControlReply, _>(&mut conn).await.unwrap()
        else {
            panic!("expected owned spawn");
        };
        (None, pid)
    };
    poll(|| ready.exists() && pid_file.exists()).await;
    let pid: i32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The temporary supervisor guard kills its owned group even on assertion failure.
    assert!(alive(pid));
    assert_ne!(pid as u32, leader);
    assert_eq!(unsafe { libc::getpgid(pid) }, leader as i32);
    if !fault {
        std::fs::write(release, "exit").unwrap();
        poll(|| {
            owner
                .registry
                .debug_entry_stats(&format!("term:{term}"))
                .is_some_and(|s| s.exit_recorded)
        })
        .await;
        if let Some(entry) = &entry {
            assert!(
                entry
                    .wait_exit_persisted_for_test(Duration::from_secs(2))
                    .await
            );
        }
        assert!(alive(pid), "owned member must outlive explicit leader exit");
    } else {
        assert!(
            !owner
                .registry
                .debug_entry_stats(&format!("term:{term}"))
                .unwrap()
                .exit_recorded
        );
    }
    if let Some(proxy) = &proxy {
        for _ in 0..2 {
            assert_eq!(
                delete(&state, format!("/api/cards/{card}")).await.status(),
                StatusCode::CONFLICT
            );
            assert!(state.repo.terminal_get(&term).await.unwrap().is_some());
            assert!(state.terminal_renderer.get(&term).is_some());
            assert!(alive(pid));
            assert!(
                owner
                    .registry
                    .debug_entry_stats(&format!("term:{term}"))
                    .unwrap()
                    .exit_recorded
            );
        }
        proxy
            .reject
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
    assert_eq!(
        delete(&state, format!("/api/cards/{card}")).await.status(),
        StatusCode::NO_CONTENT
    );
    poll(|| !alive(pid)).await;
}
#[tokio::test]
async fn legacy_disposal_group_exited_present() {
    exited_group(true, false).await;
}
#[tokio::test]
async fn legacy_disposal_group_exited_missing() {
    exited_group(false, false).await;
}

#[tokio::test]
async fn legacy_disposal_recover_exit_write_same_server() {
    let owner = Owner::new();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = owner.state(repo.clone());
    let (_, _, card, term) = seed(&state).await;
    let entry = owned_entry(&state, &owner.sock, &term).await;
    repo.db().writer().execute(sqlx::query("CREATE TRIGGER reject_exit BEFORE UPDATE OF exit_code ON terminals BEGIN SELECT RAISE(ABORT,'temporary'); END")).await.unwrap();
    assert_eq!(
        delete(&state, format!("/api/cards/{card}")).await.status(),
        StatusCode::CONFLICT
    );
    assert!(state.terminal_renderer.get(&term).is_some());
    repo.db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER reject_exit"))
        .await
        .unwrap();
    assert_eq!(
        delete(&state, format!("/api/cards/{card}")).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(
        entry
            .wait_exit_persisted_for_test(Duration::from_secs(1))
            .await
    );
}

#[tokio::test]
async fn legacy_disposal_group_kill_retry_exited_alias() {
    exited_group(true, true).await;
}

async fn completion_recovery(failure: &str, absent: bool, scheduler_first: bool) {
    let owner = Owner::with_grace(if absent {
        Duration::from_millis(5)
    } else {
        Duration::from_secs(60)
    });
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = owner.state(repo.clone());
    let (_, track, card, term) = seed(&state).await;
    state
        .terminal_renderer
        .set_hook_settings_dir(owner._dir.path().into());
    let hook_file = owner._dir.path().join(format!("{card}.json"));
    std::fs::write(&hook_file, "retained").unwrap();
    state
        .terminal_renderer
        .set_task_hook(calm_server::scheduler::TerminalTaskHook::new(
            repo.clone(),
            state.events.clone(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        ));
    repo.db().writer().execute(sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.idempotency_key','recovery-task') WHERE id=?1"
    ).bind(&card)).await.unwrap();
    let ready = owner._dir.path().join("output-ready");
    let release = owner._dir.path().join("release-exit");
    let ordinary_exit = failure == "lookup";
    let ending = if ordinary_exit {
        format!(
            "while [ ! -e '{}' ]; do sleep 0.01; done; exit 23",
            release.display()
        )
    } else {
        "exec sleep 60".into()
    };
    let entry = state
        .terminal_renderer
        .ensure(calm_server::terminal_renderer::RendererConfig {
            terminal_id: term.clone(),
            cols: 80,
            rows: 24,
            buffer_bytes: 8192,
            terminal_fg: (220, 220, 220),
            terminal_bg: (15, 20, 24),
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                format!(
                    "echo completion-evidence; touch '{}'; {ending}",
                    ready.display()
                ),
            ],
            envs: vec![],
            cwd: "/tmp".into(),
            supervisor_sock: owner.sock.clone(),
        })
        .await
        .unwrap();
    poll(|| ready.exists()).await;
    repo.db().writer().execute(sqlx::query(
        "INSERT INTO tasks(id,track_id,key,kind,goal,context_json,status,worker_card_id,created_at_ms,\
         updated_at_ms) VALUES('recovery-task',?1,'recovery-task','terminal','true','null','running',?2,1,1)"
    ).bind(&track).bind(&card)).await.unwrap();

    seed_session(&repo, &card, &term).await;
    let read_table = match failure {
        "lookup" => Some("terminals"),
        "classification" | "ownership" => Some("workspace_executions"),
        _ => None,
    };
    if let Some(table) = read_table {
        repo.db()
            .writer()
            .execute(sqlx::query(&format!(
                "ALTER TABLE {table} RENAME TO unavailable_completion_query"
            )))
            .await
            .unwrap();
        // Exit comes from the real temporary supervisor, after the query is unavailable.
        if ordinary_exit {
            std::fs::write(&release, "exit").unwrap();
        } else {
            entry
                .shutdown_signal(calm_session::control::ProcSignal::Term)
                .await;
        }
        assert!(!entry.wait_exit_reader_closed_for_test().await);
        repo.db()
            .writer()
            .execute(sqlx::query(&format!(
                "ALTER TABLE unavailable_completion_query RENAME TO {table}"
            )))
            .await
            .unwrap();
    }
    if matches!(failure, "typed" | "ownership") {
        seed_typed_history(&repo, &track, &term, owner._dir.path()).await;
        if failure == "typed" {
            entry
                .shutdown_signal(calm_session::control::ProcSignal::Term)
                .await;
            assert!(!entry.wait_exit_reader_closed_for_test().await);
        }
        // Initial positive classification and ownership acquired before retry both keep
        // Store as the sole writer. Router admission must not fall back to legacy disposal.
        for _ in 0..2 {
            assert_eq!(
                delete(&state, format!("/api/cards/{card}")).await.status(),
                StatusCode::CONFLICT
            );
            let terminal = state.repo.terminal_get(&term).await.unwrap().unwrap();
            assert!(terminal.is_typed(state.repo.as_ref()).await.unwrap());
            assert_eq!(terminal.exit_code, None);
            assert!(!terminal.signal_killed);
            assert!(terminal.pty_output.is_empty());
            let session: String =
                sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id='recovery-session'")
                    .fetch_one(repo.db().read_pool())
                    .await
                    .unwrap();
            assert_eq!(session, "running");
            assert_eq!(
                state
                    .repo
                    .task_get("recovery-task")
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                calm_server::model::TaskStatus::Running
            );
            assert!(
                !state
                    .repo
                    .events_since(0, i64::MAX)
                    .await
                    .unwrap()
                    .iter()
                    .any(|(_, _, _, event)| matches!(
                        event.kind_tag(),
                        "task.failed" | "task.completed"
                    ))
            );
            assert!(hook_file.exists());
            assert!(Arc::ptr_eq(
                &entry,
                &state.terminal_renderer.get(&term).unwrap()
            ));
        }
        return;
    }
    let trigger = match failure {
        "terminal" => {
            "CREATE TRIGGER reject_completion BEFORE UPDATE OF exit_code ON terminals BEGIN SELECT RAISE(ABORT,'temporary'); END"
        }
        "session" => {
            "CREATE TRIGGER reject_completion BEFORE UPDATE OF state ON worker_sessions BEGIN SELECT RAISE(ABORT,'temporary'); END"
        }
        "hook" => {
            "CREATE TRIGGER reject_completion BEFORE UPDATE OF status ON tasks BEGIN SELECT RAISE(ABORT,'temporary'); END"
        }
        "lookup" | "classification" => {
            "CREATE TRIGGER reject_completion BEFORE UPDATE OF exit_code ON terminals BEGIN SELECT RAISE(ABORT,'temporary'); END"
        }
        _ => unreachable!(),
    };
    repo.db()
        .writer()
        .execute(sqlx::query(trigger))
        .await
        .unwrap();
    assert_eq!(
        delete(&state, format!("/api/cards/{card}")).await.status(),
        StatusCode::CONFLICT
    );
    assert!(!entry.wait_exit_reader_closed_for_test().await);
    assert!(state.terminal_renderer.get(&term).is_some());
    assert!(state.repo.terminal_get(&term).await.unwrap().is_some());
    assert!(hook_file.exists());
    if absent {
        poll(|| {
            owner
                .registry
                .debug_entry_stats(&format!("term:{term}"))
                .is_none()
        })
        .await;
    } else {
        assert!(
            owner
                .registry
                .debug_entry_stats(&format!("term:{term}"))
                .is_some_and(|s| s.exit_recorded)
        );
    }
    repo.db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER reject_completion"))
        .await
        .unwrap();
    if scheduler_first {
        state.scheduler_for_test().sweep_all().await;
        let task = state.repo.task_get("recovery-task").await.unwrap().unwrap();
        assert!(
            task.status.is_terminal(),
            "canonical scheduler must finish before DELETE retries"
        );
        assert!(
            !entry
                .wait_exit_persisted_for_test(Duration::from_millis(20))
                .await
        );
    }
    // Inspect completion's durable outcome before the final deleting commit, and verify
    // a subsequent retry emits no duplicate task completion and retains the hook until commit.
    repo.db().writer().execute(sqlx::query(
        "CREATE TRIGGER reject_delete BEFORE DELETE ON cards BEGIN SELECT RAISE(ABORT,'commit'); END"
    )).await.unwrap();
    assert_eq!(
        delete(&state, format!("/api/cards/{card}")).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(
        entry
            .wait_exit_persisted_for_test(Duration::from_secs(1))
            .await
    );
    let terminal = state.repo.terminal_get(&term).await.unwrap().unwrap();
    assert_eq!(terminal.signal_killed, !ordinary_exit);
    assert_eq!(terminal.exit_code, ordinary_exit.then_some(23));
    assert_eq!(terminal.pty_output, "completion-evidence\r\n");
    assert!(!terminal.pty_output_truncated);
    let session: String =
        sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id='recovery-session'")
            .fetch_one(repo.db().read_pool())
            .await
            .unwrap();
    assert_eq!(session, if ordinary_exit { "exited" } else { "failed" });
    let completion_count = || async {
        state
            .repo
            .events_since(0, i64::MAX)
            .await
            .unwrap()
            .into_iter()
            .filter(|(_, _, _, event)| event.kind_tag() == "task.failed")
            .count()
    };
    assert_eq!(completion_count().await, 1);
    assert!(hook_file.exists());
    repo.db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER reject_delete"))
        .await
        .unwrap();
    assert_eq!(
        delete(&state, format!("/api/cards/{card}")).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(completion_count().await, 1);
    assert!(!hook_file.exists());
}

#[tokio::test]
async fn legacy_disposal_recover_terminal_sticky_and_absent() {
    for absent in [false, true] {
        completion_recovery("terminal", absent, false).await;
    }
}
#[tokio::test]
async fn legacy_disposal_recover_session_sticky_and_absent() {
    for absent in [false, true] {
        completion_recovery("session", absent, false).await;
    }
}
#[tokio::test]
async fn legacy_disposal_recover_hook_sticky_and_absent() {
    for absent in [false, true] {
        completion_recovery("hook", absent, false).await;
    }
}
#[tokio::test]
async fn legacy_disposal_recover_hook_scheduler_first() {
    for absent in [false, true] {
        completion_recovery("hook", absent, true).await;
    }
}

#[tokio::test]
async fn legacy_disposal_retry_disconnected_without_exit_refused() {
    let owner = Owner::new();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = owner.state(repo);
    let (_, _, card, term) = seed(&state).await;
    let entry = owned_entry(&state, &owner.sock, &term).await;
    entry.disconnect_output_source_for_test();
    owner.task.abort();
    tokio::task::yield_now().await;
    for _ in 0..2 {
        assert_eq!(
            delete(&state, format!("/api/cards/{card}")).await.status(),
            StatusCode::CONFLICT
        );
        assert!(state.repo.terminal_get(&term).await.unwrap().is_some());
        assert!(state.terminal_renderer.get(&term).is_some());
    }
}

async fn seed_session(repo: &SqlxRepo, card: &str, terminal: &str) {
    use calm_server::session_projection_repo::{
        WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    repo.db()
        .writer()
        .with_writer(|writer| {
            let card = card.to_owned();
            let terminal = terminal.to_owned();
            Box::pin(async move {
                let mut tx = writer.begin_immediate().await.unwrap();
                calm_truth::db::sqlite::session_start_runtime_tx(
                    &mut tx,
                    WorkerSessionInit {
                        id: "recovery-session".into(),
                        card_id: card,
                        kind: WorkerSessionKind::Terminal,
                        agent_provider: None,
                        status: WorkerSessionState::Running,
                        terminal_run_id: Some(terminal),
                        thread_id: None,
                        session_id: None,
                        active_turn_id: None,
                        handle_state_json: None,
                        spawn_op_id: None,
                        now_ms: calm_server::model::now_ms(),
                    },
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn legacy_disposal_recover_lookup_sticky() {
    completion_recovery("lookup", false, false).await;
}
#[tokio::test]
async fn legacy_disposal_recover_lookup_absent() {
    completion_recovery("lookup", true, false).await;
}
#[tokio::test]
async fn legacy_disposal_recover_classification_sticky() {
    completion_recovery("classification", false, false).await;
}
#[tokio::test]
async fn legacy_disposal_recover_classification_absent() {
    completion_recovery("classification", true, false).await;
}

async fn seed_typed_history(repo: &SqlxRepo, track: &str, terminal: &str, root: &std::path::Path) {
    let mut intent = workspace_runtime::execution::test_support::intent(root.to_owned());
    intent.metadata.track.0 = track.to_owned();
    intent.metadata.alias = workspace_runtime::execution::ProcessAlias(
        calm_truth::db::sqlite::workspace_execution::terminal_alias(terminal),
    );
    repo.db()
        .writer()
        .with_writer(|writer| {
            Box::pin(async move {
                let mut tx = writer.begin_immediate().await.unwrap();
                calm_truth::db::sqlite::workspace_execution::insert(
                    &mut tx,
                    intent.metadata.id.as_str(),
                    &intent.metadata.track.0,
                    &intent.metadata.alias.0,
                    &serde_json::to_string(&intent.metadata).unwrap(),
                    &serde_json::to_string(&intent.secrets).unwrap(),
                )
                .await
                .unwrap();
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn legacy_disposal_recover_classification_typed() {
    completion_recovery("typed", false, false).await;
}
#[tokio::test]
async fn legacy_disposal_recover_classification_ownership_change() {
    completion_recovery("ownership", false, false).await;
}
