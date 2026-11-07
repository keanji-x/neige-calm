//! Admission errors retain the installed source; existing connections need no new admission.
use super::*;

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn unreadable_record_refuses_before_cleanup_or_source_consumption() {
    let _env = ENV_LOCK.lock().await;
    for entry in [Entry::Start, Entry::Detached, Entry::Replace] {
        let mut fixture = Fixture::new(false, "default").await;
        let daemon = fixture.daemon().await;
        fixture.invoke(&daemon, Entry::Start).await.unwrap();
        let before = fixture.row().await;
        let pid = before.pid.unwrap();
        let metadata = std::fs::metadata(&fixture.paths.socket).unwrap();
        let leaked_env = fixture.paths.home.join(".env");
        std::fs::write(&leaked_env, "MUST_RETAIN_UNTIL_ADMISSION=1\n").unwrap();

        // A closed pool makes the real repository read fail without changing the row.
        fixture.repo.db().close().await;
        let result = tokio::time::timeout(Duration::from_secs(15), entry.call(&daemon))
            .await
            .expect("unreadable record must refuse promptly");
        fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());

        assert!(result.is_err(), "{entry:?}: {result:?}");
        assert!(leaked_env.exists(), "admission must precede home cleanup");
        assert_eq!(fixture.exec.pids(), vec![pid]);
        assert_eq!(read_proc_start_time(pid), before.process_start_time);
        assert_eq!(daemon.status_snapshot().state, SharedDaemonState::Running);
        assert_eq!(daemon.status_snapshot().runtime.unwrap().pid, pid);
        assert!(daemon.readiness_receiver().borrow().running);
        assert!(!daemon.heal_active_for_test());
        assert!(!PathBuf::from(format!("{}.term.{pid}", fixture.exec.prefix.display())).exists());
        let after = std::fs::metadata(&fixture.paths.socket).unwrap();
        assert_eq!((after.dev(), after.ino()), (metadata.dev(), metadata.ino()));
        assert!(
            tokio::net::UnixStream::connect(&fixture.paths.socket)
                .await
                .is_ok()
        );
        assert_same_row(&before, &fixture.row().await);
    }
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn recorded_connected_client_and_noop_preconditions_keep_the_running_source() {
    let _env = ENV_LOCK.lock().await;
    let fixture = Fixture::new(false, "default").await;
    let daemon = fixture.daemon().await;
    fixture.invoke(&daemon, Entry::Start).await.unwrap();
    fixture.claim_installed_child().await;
    let before = fixture.row().await;
    let generation = daemon.generation_for_test().await;
    let methods_before = optional_text(&fixture.paths.socket.with_extension("methods"));

    daemon.ensure_running().await.unwrap();
    // Generation equality is a precondition, not evidence that this live child died.
    for pre in [
        ReplacePrecondition::Always,
        ReplacePrecondition::GenerationIs(generation),
    ] {
        assert!(matches!(
            daemon
                .transition_replace_for_test("refused live source", pre)
                .await,
            Err(CalmError::Conflict(_))
        ));
    }
    for pre in [
        ReplacePrecondition::NotRunning,
        ReplacePrecondition::GenerationIs(generation.wrapping_add(1)),
    ] {
        assert_eq!(
            daemon
                .transition_replace_for_test("no effect", pre)
                .await
                .unwrap(),
            calm_server::shared_codex_appserver::ReplaceOutcome::PreconditionFailed
        );
    }
    daemon
        .model_list(tokio::time::Instant::now() + Duration::from_secs(2))
        .await
        .unwrap();

    assert_eq!(
        optional_text(&fixture.paths.socket.with_extension("methods")),
        format!("{methods_before}model/list\n"),
        "the existing connection must serve the request without another initialize"
    );
    assert_eq!(fixture.exec.pids(), vec![before.pid.unwrap()]);
    assert_eq!(
        read_proc_start_time(before.pid.unwrap()),
        before.process_start_time
    );
    assert_eq!(
        daemon.status_snapshot().runtime.unwrap().pid,
        before.pid.unwrap()
    );
    assert!(daemon.readiness_receiver().borrow().running);
    assert!(!daemon.heal_active_for_test());
    assert_same_row(&before, &fixture.row().await);
}

async fn crashed_source_refusal(adopted: bool, unreadable: bool) {
    let mut fixture = Fixture::new(false, "default").await;
    // The crash watcher runs promptly; persistent-failure healing stays quiet.
    fixture
        .config
        .shared_codex_appserver_restart_initial_delay_ms = 1;
    fixture.config.shared_codex_appserver_restart_max_delay_ms = 60_000;
    let first = fixture.daemon().await;
    fixture.invoke(&first, Entry::Start).await.unwrap();
    let pid = fixture.row().await.pid.unwrap();
    let mut survivor = Command::new("sleep")
        .arg("120")
        .process_group(pid)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let daemon = if adopted {
        drop(first);
        let next = fixture.daemon().await;
        next.start_or_takeover().await.unwrap();
        assert!(next.taken_over_pid_watcher_active_for_test().await);
        next
    } else {
        first
    };
    if !unreadable {
        fixture.claim_installed_child().await;
    }
    let before = fixture.row().await;
    let metadata = std::fs::metadata(&fixture.paths.socket).unwrap();
    if unreadable {
        fixture.repo.db().close().await;
    }
    // Kill only the installed leader; its independent group member must survive refusal.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if daemon.status_snapshot().state == SharedDaemonState::Failed
                && daemon.heal_active_for_test()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("confirmed death must revoke Running and arm healing despite refusal");
    assert!(!daemon.readiness_receiver().borrow().running);
    assert!(daemon.ensure_running().await.is_err());
    if unreadable {
        fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    }
    assert_same_row(&before, &fixture.row().await);
    assert_eq!(fixture.exec.pids(), vec![pid]);
    assert!(survivor.try_wait().unwrap().is_none());
    let after = std::fs::metadata(&fixture.paths.socket).expect("endpoint must remain");
    assert_eq!((after.dev(), after.ino()), (metadata.dev(), metadata.ino()));
    assert!(!PathBuf::from(format!("{}.term.{pid}", fixture.exec.prefix.display())).exists());
    survivor.kill().await.unwrap();
    survivor.wait().await.unwrap();
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn spawned_crash_phase_and_read_refusals_revoke_only_memory() {
    let _env = ENV_LOCK.lock().await;
    for unreadable in [false, true] {
        crashed_source_refusal(false, unreadable).await;
    }
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn adopted_crash_phase_and_read_refusals_revoke_only_memory() {
    let _env = ENV_LOCK.lock().await;
    for unreadable in [false, true] {
        crashed_source_refusal(true, unreadable).await;
    }
}
