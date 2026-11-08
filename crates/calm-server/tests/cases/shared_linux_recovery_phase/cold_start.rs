//! Cold admission owns memory completion without gaining process or durable authority.
use super::*;

#[derive(Clone, Copy, Debug)]
enum ColdEntry {
    Boot,
    Ensure,
    DetachedBoot,
    CancelledDetachedBoot,
}

fn boot_state(
    fixture: &Fixture,
    daemon: Arc<SharedCodexAppServer>,
) -> calm_server::state::AppState {
    let events = calm_server::event::EventBus::new();
    calm_server::state::AppState::from_parts(
        fixture.repo.clone(),
        events.clone(),
        Arc::new(calm_server::state::DaemonClient::new_stub()),
        Arc::new(calm_server::plugin_host::PluginHost::new_full(
            Arc::new(calm_server::plugin_host::PluginRegistry::empty()),
            fixture.repo.clone(),
            fixture.root.path().join("plugins"),
            fixture.root.path().join("plugins-data"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(calm_server::state::CodexClient::new_stub()),
        None,
        None,
    )
    .with_shared_codex_appserver(daemon)
}

#[derive(Debug)]
struct Refusal {
    state: SharedDaemonState,
    ready: bool,
    heal: bool,
    execs: Vec<i32>,
    endpoint: witness::EndpointEffects,
}

async fn cold_admission(entry: ColdEntry) {
    let mut fixture = Fixture::new(false, "default").await;
    fixture
        .config
        .shared_codex_appserver_restart_initial_delay_ms = 1;
    fixture.config.shared_codex_appserver_restart_max_delay_ms = 200;
    fixture
        .write(fixture.update(RecoveryWrite::Legacy, SharedDaemonState::Idle))
        .await;
    let before = fixture.row().await;
    let daemon = fixture.daemon().await;
    assert_eq!(daemon.status_snapshot().state, SharedDaemonState::Idle);
    let state = Arc::new(boot_state(&fixture, daemon.clone()));
    let erased: Arc<dyn Repo> = fixture.repo.clone();
    let isolation = BackendIsolation::prepare(&fixture.config, &erased)
        .await
        .unwrap();
    let mut writer = sqlx::SqliteConnection::connect(&fixture.url).await.unwrap();
    let detached = matches!(
        entry,
        ColdEntry::DetachedBoot | ColdEntry::CancelledDetachedBoot
    );
    let (mut arrival, mut release) = if detached {
        let (arrival, release) = daemon.pause_detached_admission_for_test();
        (Some(arrival), Some(release))
    } else {
        (None, None)
    };
    let mut caller = None;
    let mut serial = None;
    let mut endpoint = None;
    let mut fault_applied = false;
    let observation: Result<Refusal, String> = async {
        if detached {
            caller = Some(tokio::spawn({
                let state = state.clone();
                async move { calm_server::boot_harnesses(&state).await }
            }));
            tokio::time::timeout(Duration::from_secs(5), arrival.take().unwrap())
                .await
                .map_err(|_| "real boot never reached detached admission".to_owned())?
                .map_err(|error| error.to_string())?;
        }
        endpoint = Some(LiveEndpoint::new(fixture.root.path(), &fixture.paths.socket).await);
        sqlx::query("ALTER TABLE codex_backend_daemons RENAME TO paused_daemons")
            .execute(&mut writer)
            .await
            .map_err(|error| error.to_string())?;
        fault_applied = true;
        // Prove that only runtime admission is unavailable, including the real
        // read entry points used by the preceding isolation/cache rebuild.
        isolation
            .verify_backend(erased.as_ref(), &fixture.backend)
            .await
            .map_err(|error| error.to_string())?;
        calm_server::session_projection_lookup::merge_active_shared_thread_attribution(
            fixture.repo.as_ref(),
        )
        .await
        .map_err(|error| error.to_string())?;
        calm_truth::db::sqlite::session_backend::live_bound_threads(
            fixture.repo.db().read_pool(),
            "codex",
        )
        .await
        .map_err(|error| error.to_string())?;
        if fixture
            .repo
            .shared_daemon_runtime_get("default")
            .await
            .is_ok()
        {
            return Err("real runtime read unexpectedly remained available".into());
        }
        if matches!(entry, ColdEntry::CancelledDetachedBoot) {
            let task = caller.as_mut().unwrap();
            task.abort();
            if !matches!(task.await, Err(error) if error.is_cancelled()) {
                return Err("boot caller was not cancelled at detached admission".into());
            }
            caller = None;
        }
        if let Some(release) = release.take() {
            release
                .send(())
                .map_err(|_| "detached task lost its release".to_owned())?;
        }
        if !detached {
            caller = Some(tokio::spawn({
                let state = state.clone();
                async move {
                    if matches!(entry, ColdEntry::Ensure) {
                        state
                            .shared_codex_appserver
                            .ensure_running()
                            .await
                            .map(|()| 0)
                    } else {
                        calm_server::boot_harnesses(&state).await
                    }
                }
            }));
        }
        if let Some(task) = caller.as_mut() {
            let result = tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .map_err(|_| "entry never returned from admission".to_owned())?
                .map_err(|error| error.to_string())?;
            caller = None;
            match (entry, result) {
                (ColdEntry::Ensure, Err(error)) if error.code() == "codex_app_server" => {}
                (ColdEntry::Boot | ColdEntry::DetachedBoot, Ok(0)) => {}
                (_, result) => return Err(format!("unexpected entry result: {result:?}")),
            }
        }
        // Leave the real fault in place through an automatic retry opportunity.
        tokio::time::sleep(Duration::from_millis(350)).await;
        serial = Some(
            tokio::time::timeout(
                Duration::from_secs(5),
                daemon.lock_transition_serial_for_test(),
            )
            .await
            .map_err(|_| "admission did not release the serial".to_owned())?,
        );
        Ok(Refusal {
            state: daemon.status_snapshot().state,
            ready: daemon.readiness_receiver().borrow().running,
            heal: daemon.heal_active_for_test(),
            execs: fixture.exec.pids(),
            endpoint: endpoint.as_mut().unwrap().effects().await,
        })
    }
    .await;

    // Repair and release every seam even if the observation failed. Do not make
    // a regression assertion until the independent and spawned children are reaped.
    drop(release.take());
    if let Some(task) = caller.take() {
        task.abort();
        let _ = task.await;
    }
    let repair = if fault_applied {
        sqlx::query("ALTER TABLE paused_daemons RENAME TO codex_backend_daemons")
            .execute(&mut writer)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    } else {
        Ok(())
    };
    let refused_row = fixture.repo.shared_daemon_runtime_get("default").await;
    let endpoint_identity = endpoint.as_ref().map(|live| (live.pid, live.birth));
    drop(endpoint.take());
    let endpoint_reaped =
        endpoint_identity.is_none_or(|(pid, birth)| read_proc_start_time(pid) != Some(birth));
    drop(serial.take());
    let recovered = if observation.is_ok() && repair.is_ok() {
        tokio::time::timeout(Duration::from_secs(5), async {
            // Capture the exec as it happens, before waiting for Running.
            fixture.exec.capture_new_exec(0).await;
            loop {
                if daemon.status_snapshot().state == SharedDaemonState::Running
                    && !daemon.heal_active_for_test()
                {
                    break fixture.repo.shared_daemon_runtime_get("default").await;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .ok()
        .and_then(Result::ok)
    } else {
        None
    };
    let recovered_status = daemon.status_snapshot();
    let recovered_ready = *daemon.readiness_receiver().borrow();
    let recovered_birth = recovered
        .as_ref()
        .and_then(|row| row.pid.and_then(read_proc_start_time));
    let execs = fixture.exec.pids();
    let cleanup_serial = tokio::time::timeout(
        Duration::from_secs(5),
        daemon.lock_transition_serial_for_test(),
    )
    .await;
    if cleanup_serial.is_err() {
        daemon.abort_detached_spawn_transition_for_test();
    }
    fixture.repo.db().close().await;
    drop(state);
    drop(daemon);
    let remaining = fixture.exec.reap_captured();
    drop(cleanup_serial);
    eprintln!(
        "{entry:?}: repair={repair:?}, endpoint_reaped={endpoint_reaped}, remaining={remaining:?}, refusal={observation:?}, recovered={recovered:?}"
    );
    assert!(repair.is_ok(), "schema restoration failed: {repair:?}");
    assert!(endpoint_reaped, "independent fixture child was not reaped");
    assert!(
        remaining.is_empty(),
        "unreaped captured children: {remaining:?}"
    );
    assert_same_row(&before, &refused_row.expect("restored refusal row"));
    let refusal = observation.unwrap_or_else(|error| panic!("{entry:?}: {error}"));
    assert!(refusal.execs.is_empty(), "refusal execs: {refusal:?}");
    let effects = &refusal.endpoint;
    assert!(
        effects.alive
            && effects.no_term
            && effects.same_inode
            && effects.connectable
            && effects.no_requests,
        "refusal acquired endpoint authority: {refusal:?}"
    );
    assert_eq!(
        refusal.state,
        SharedDaemonState::Failed,
        "{entry:?}: {refusal:?}"
    );
    assert!(
        !refusal.ready,
        "cold refusal published readiness: {refusal:?}"
    );
    assert!(
        refusal.heal,
        "cold refusal did not arm automatic recovery: {refusal:?}"
    );
    let row =
        recovered.expect("already-armed heal must recover without ensure, schedule_heal or nudge");
    assert_eq!(row.state, SharedDaemonState::Running);
    assert_eq!(recovered_status.state, SharedDaemonState::Running);
    assert_eq!(row.pid, recovered_status.runtime.map(|runtime| runtime.pid));
    assert!(
        recovered_birth.is_some(),
        "recovered child had no native identity"
    );
    assert_eq!(row.process_start_time, recovered_birth);
    assert!(recovered_ready.running);
    assert_eq!(execs, vec![row.pid.unwrap()]);
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn boot_harnesses_runtime_read_refusal_terminalizes_and_heals() {
    let _env = ENV_LOCK.lock().await;
    cold_admission(ColdEntry::Boot).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn first_ensure_runtime_read_refusal_terminalizes_and_heals() {
    let _env = ENV_LOCK.lock().await;
    cold_admission(ColdEntry::Ensure).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn cold_boot_detached_read_refusal_terminalizes_and_heals() {
    let _env = ENV_LOCK.lock().await;
    cold_admission(ColdEntry::DetachedBoot).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn cancelled_cold_boot_detached_read_refusal_terminalizes_and_heals() {
    let _env = ENV_LOCK.lock().await;
    cold_admission(ColdEntry::CancelledDetachedBoot).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn healthy_runtime_read_refusal_and_noop_preserve_running() {
    let _env = ENV_LOCK.lock().await;
    let fixture = Fixture::new(false, "default").await;
    let daemon = fixture.daemon().await;
    fixture.invoke(&daemon, Entry::Start).await.unwrap();
    let before = fixture.row().await;
    let pid = before.pid.unwrap();
    let readiness = *daemon.readiness_receiver().borrow();
    let metadata = std::fs::metadata(&fixture.paths.socket).unwrap();
    let methods = optional_text(&fixture.paths.socket.with_extension("methods"));
    let mut writer = sqlx::SqliteConnection::connect(&fixture.url).await.unwrap();
    sqlx::query("ALTER TABLE codex_backend_daemons RENAME TO paused_daemons")
        .execute(&mut writer)
        .await
        .unwrap();
    let mut snapshots = Vec::new();
    let mut serial = None;
    let observation: Result<(), String> = async {
        for entry in [Entry::Start, Entry::Replace, Entry::Ensure] {
            let result = tokio::time::timeout(Duration::from_secs(5), entry.call(&daemon))
                .await
                .map_err(|_| format!("{entry:?} did not return"))?;
            snapshots.push((
                entry,
                result,
                daemon.status_snapshot(),
                *daemon.readiness_receiver().borrow(),
                daemon.heal_active_for_test(),
            ));
        }
        let outcome = daemon
            .transition_replace_for_test("healthy no-op", ReplacePrecondition::NotRunning)
            .await
            .map_err(|error| error.to_string())?;
        if outcome != ReplaceOutcome::PreconditionFailed {
            return Err(format!("NotRunning accepted a healthy source: {outcome:?}"));
        }
        serial = Some(
            tokio::time::timeout(
                Duration::from_secs(5),
                daemon.lock_transition_serial_for_test(),
            )
            .await
            .map_err(|_| "healthy refusal did not release serial".to_owned())?,
        );
        Ok(())
    }
    .await;
    let repair = sqlx::query("ALTER TABLE paused_daemons RENAME TO codex_backend_daemons")
        .execute(&mut writer)
        .await;
    let after = fixture.repo.shared_daemon_runtime_get("default").await;
    let birth = read_proc_start_time(pid);
    let execs = fixture.exec.pids();
    let no_term = !PathBuf::from(format!("{}.term.{pid}", fixture.exec.prefix.display())).exists();
    let inode = std::fs::metadata(&fixture.paths.socket)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()));
    let connectable = tokio::net::UnixStream::connect(&fixture.paths.socket)
        .await
        .is_ok();
    let methods_after = optional_text(&fixture.paths.socket.with_extension("methods"));
    fixture.repo.db().close().await;
    drop(daemon);
    let remaining = fixture.exec.reap_captured();
    drop(serial);
    eprintln!("healthy: repair={repair:?}, remaining={remaining:?}, observation={observation:?}");
    assert!(repair.is_ok(), "restore healthy fixture schema: {repair:?}");
    assert!(
        remaining.is_empty(),
        "unreaped healthy children: {remaining:?}"
    );
    observation.expect("healthy admission observation");
    assert_same_row(&before, &after.expect("restored healthy row"));
    assert_eq!(execs, vec![pid]);
    assert_eq!(birth, before.process_start_time);
    assert!(no_term && connectable);
    assert_eq!(inode, Some((metadata.dev(), metadata.ino())));
    assert_eq!(methods_after, methods);
    for (entry, result, status, ready, heal) in snapshots {
        match entry {
            Entry::Start | Entry::Replace => assert!(
                matches!(result, Err(CalmError::CodexAppServer(_))),
                "{entry:?}: {result:?}"
            ),
            Entry::Ensure => assert!(result.is_ok(), "{entry:?}: {result:?}"),
            Entry::Detached => unreachable!(),
        }
        assert_eq!(
            status.state,
            SharedDaemonState::Running,
            "{entry:?}: {status:?}"
        );
        assert_eq!(status.runtime.unwrap().pid, pid);
        assert_eq!(ready, readiness);
        assert!(!heal, "healthy refusal armed heal: {entry:?}");
    }
}
