//! A refused later admission must finish the already-consumed memory lifecycle.
use super::*;

#[derive(Clone, Copy, Debug)]
enum Source {
    Replace,
    OwnedCrash,
    AdoptedCrash,
}

#[derive(Clone, Copy, Debug)]
enum Window {
    StartBody,
    Detached,
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    Read,
    Phase,
    Path,
}

#[derive(Debug)]
struct Refusal {
    code: Option<&'static str>,
    state: SharedDaemonState,
    ready: bool,
    heal: bool,
    execs: Vec<i32>,
    endpoint: witness::EndpointEffects,
}

async fn post_consumption(source: Source, window: Window, fault: Fault, cancel: bool) {
    let mut fixture = Fixture::new(matches!(fault, Fault::Path), "default").await;
    fixture
        .config
        .shared_codex_appserver_restart_initial_delay_ms = 1;
    fixture.config.shared_codex_appserver_restart_max_delay_ms = 200;
    let first = fixture.daemon().await;
    fixture.invoke(&first, Entry::Start).await.unwrap();
    let daemon = if matches!(source, Source::AdoptedCrash) {
        drop(first);
        let next = fixture.daemon().await;
        next.start_or_takeover().await.unwrap();
        assert!(next.taken_over_pid_watcher_active_for_test().await);
        next
    } else {
        first
    };
    let before = fixture.row().await;
    let old_pid = before.pid.unwrap();
    assert_eq!(daemon.status_snapshot().state, SharedDaemonState::Running);
    assert!(daemon.readiness_receiver().borrow().running);
    assert!(!daemon.heal_active_for_test());
    assert_eq!(fixture.exec.pids(), vec![old_pid]);
    assert_eq!(read_proc_start_time(old_pid), before.process_start_time);

    let mut writer = sqlx::SqliteConnection::connect(&fixture.url).await.unwrap();
    let mut entry_gate = Some(daemon.transition_entry_gate_for_test().lock_owned().await);
    let (mut arrival, mut release) = if matches!(window, Window::Detached) {
        let (arrival, release) = daemon.pause_detached_admission_for_test();
        (Some(arrival), Some(release))
    } else {
        (None, None)
    };
    let mut readiness = daemon.readiness_receiver();
    let mut caller = if matches!(source, Source::Replace) {
        Some(tokio::spawn({
            let daemon = daemon.clone();
            async move {
                daemon
                    .transition_replace_for_test(
                        "post-consumption refusal",
                        ReplacePrecondition::Always,
                    )
                    .await
            }
        }))
    } else {
        // The real watcher must positively observe this fixture leader's death.
        assert_eq!(unsafe { libc::kill(old_pid, libc::SIGKILL) }, 0);
        None
    };
    let mut endpoint = None;
    let mut serial = None;
    let mut fault_applied = false;
    let mut expected = before.clone();

    // Every fallible operation after changing the DB returns here. Restore the
    // real table/phase and reap resources below before making any red assertion.
    let observation: Result<Refusal, String> = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let running = readiness.borrow_and_update().running;
                if !running {
                    break;
                }
                readiness
                    .changed()
                    .await
                    .map_err(|error| error.to_string())?;
            }
            Ok::<_, String>(())
        })
        .await
        .map_err(|_| "replacement never consumed the source".to_owned())??;
        let consumed = daemon.status_snapshot();
        if consumed.state != SharedDaemonState::Restarting || consumed.runtime.is_some() {
            return Err(format!(
                "entry did not consume its Running source: {consumed:?}"
            ));
        }
        if let Some(arrival) = arrival.take() {
            // The first and second real reads, old-source cleanup and detachment
            // finish before this handshake; only the third read is still ahead.
            drop(entry_gate.take());
            tokio::time::timeout(Duration::from_secs(5), arrival)
                .await
                .map_err(|_| "detached admission never arrived".to_owned())?
                .map_err(|error| error.to_string())?;
        }

        // Rebind only our fixture's socket. The independent listener must survive
        // the refusal even though cleanup of the old source was already allowed.
        std::fs::remove_file(&fixture.paths.socket).map_err(|error| error.to_string())?;
        endpoint = Some(LiveEndpoint::new(fixture.root.path(), &fixture.paths.socket).await);
        match fault {
            Fault::Read => {
                sqlx::query("ALTER TABLE codex_backend_daemons RENAME TO paused_daemons")
                    .execute(&mut writer)
                    .await
                    .map_err(|error| error.to_string())?;
                fault_applied = true;
            }
            Fault::Phase | Fault::Path => {
                // Preserve the real identity while refusing its phase or backend path.
                let recovery = match fault {
                    Fault::Phase => RecoveryWrite::ClaimLegacy {
                        id: RecoveryId::fresh(),
                    },
                    Fault::Path => RecoveryWrite::Legacy,
                    Fault::Read => unreachable!(),
                };
                let mut update = fixture.update(recovery, before.state);
                update.pid = before.pid;
                update.pgid = before.pgid;
                update.process_start_time = before.process_start_time;
                update.boot_id = before.boot_id.clone();
                update.started_at = before.started_at;
                update.daemon_env_signature = before.daemon_env_signature.clone();
                if matches!(fault, Fault::Path) {
                    update.codex_home_path = Some(
                        fixture
                            .root
                            .path()
                            .join("other-home")
                            .as_os_str()
                            .as_bytes()
                            .to_vec(),
                    );
                }
                fixture
                    .repo
                    .shared_daemon_runtime_set("default", update)
                    .await
                    .map_err(|error| error.to_string())?;
                fault_applied = true;
                expected = fixture
                    .repo
                    .shared_daemon_runtime_get("default")
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        if cancel {
            let task = caller
                .as_mut()
                .ok_or("cancel requires an ordinary caller")?;
            task.abort();
            if !matches!(task.await, Err(error) if error.is_cancelled()) {
                return Err("caller was not cancelled at the detached barrier".into());
            }
            caller = None;
        }
        drop(entry_gate.take());
        if let Some(release) = release.take() {
            release
                .send(())
                .map_err(|_| "detached task lost its release".to_owned())?;
        }
        let code = if let Some(task) = caller.as_mut() {
            let outcome = tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .map_err(|_| "replace did not return after refusal".to_owned())?
                .map_err(|error| error.to_string())?;
            caller = None;
            match outcome {
                Err(error) => Some(error.code()),
                Ok(outcome) => {
                    return Err(format!("replace accepted the refused row: {outcome:?}"));
                }
            }
        } else {
            None
        };
        // This also witnesses completion of the unobserved crash/cancelled task.
        // Hold the serial through the row/endpoint observations and DB repair so
        // an already-armed heal cannot race the no-effects assertions.
        serial = Some(
            tokio::time::timeout(
                Duration::from_secs(5),
                daemon.lock_transition_serial_for_test(),
            )
            .await
            .map_err(|_| "refusal did not release the transition serial".to_owned())?,
        );
        let state = daemon.status_snapshot().state;
        let ready = daemon.readiness_receiver().borrow().running;
        let heal = daemon.heal_active_for_test();
        let effects = endpoint.as_mut().unwrap().effects().await;
        Ok(Refusal {
            code,
            state,
            ready,
            heal,
            execs: fixture.exec.pids(),
            endpoint: effects,
        })
    }
    .await;

    // Finally path, including observation errors: never leave the real schema
    // renamed or the phase fault installed when a test assertion unwinds.
    drop(entry_gate.take());
    drop(release.take());
    let mut refused_row = None;
    let repair: Result<(), String> = async {
        if fault_applied {
            match fault {
                Fault::Read => {
                    sqlx::query("ALTER TABLE paused_daemons RENAME TO codex_backend_daemons")
                        .execute(&mut writer).await.map_err(|error| error.to_string())?;
                    refused_row = Some(fixture.repo.shared_daemon_runtime_get("default").await
                        .map_err(|error| error.to_string())?);
                }
                Fault::Phase => {
                    let snapshot = fixture.repo.shared_daemon_runtime_get("default").await;
                    // Restore the injected phase only; never overwrite a runtime tuple.
                    sqlx::query("UPDATE codex_backend_daemons SET recovery_phase = 'legacy', recovery_id = NULL WHERE backend_id = 'default'")
                        .execute(&mut writer).await.map_err(|error| error.to_string())?;
                    refused_row = Some(snapshot.map_err(|error| error.to_string())?);
                }
                Fault::Path => {
                    let snapshot = fixture.repo.shared_daemon_runtime_get("default").await;
                    // Restore only the injected path, retaining the runtime tuple.
                    sqlx::query("UPDATE codex_backend_daemons SET codex_home_path = ? WHERE backend_id = 'default'")
                        .bind(before.codex_home_path.as_deref())
                        .execute(&mut writer).await.map_err(|error| error.to_string())?;
                    refused_row = Some(snapshot.map_err(|error| error.to_string())?);
                }
            }
        }
        Ok(())
    }.await;
    // An independent endpoint is no longer a refusal witness after admission is
    // repaired. Reap it before allowing the existing heal to resume.
    let endpoint_identity = endpoint.as_ref().map(|live| (live.pid, live.birth));
    drop(endpoint.take());
    let endpoint_reaped =
        endpoint_identity.is_none_or(|(pid, birth)| read_proc_start_time(pid) != Some(birth));
    drop(serial.take());

    let recovered = if observation.is_ok() && repair.is_ok() {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = daemon.status_snapshot();
                if status.state == SharedDaemonState::Running
                    && status
                        .runtime
                        .as_ref()
                        .is_some_and(|runtime| runtime.pid != old_pid)
                    && !daemon.heal_active_for_test()
                {
                    fixture.exec.capture_new_exec(1).await;
                    return fixture.repo.shared_daemon_runtime_get("default").await;
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
    let recovered_birth = recovered
        .as_ref()
        .and_then(|row| row.pid.and_then(read_proc_start_time));
    let recovered_status = daemon.status_snapshot();
    let recovered_ready = daemon.readiness_receiver().borrow().running;
    let execs_after = fixture.exec.pids();

    // Close only this fixture pool under the serial before releasing the owner:
    // no watcher/heal can exec another child during assertion-time teardown.
    if let Some(task) = caller.take() {
        task.abort();
        let _ = task.await;
    }
    let cleanup_serial = tokio::time::timeout(
        Duration::from_secs(5),
        daemon.lock_transition_serial_for_test(),
    )
    .await;
    if cleanup_serial.is_err() {
        daemon.abort_detached_spawn_transition_for_test();
    }
    fixture.repo.db().close().await;
    let weak = Arc::downgrade(&daemon);
    drop(daemon);
    drop(cleanup_serial);
    let owner_released = tokio::time::timeout(Duration::from_secs(2), async {
        while weak.strong_count() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    let remaining = fixture.exec.reap_captured();

    eprintln!(
        "{source:?}/{window:?}/{fault:?}: repair={repair:?}, owner_released={owner_released}, \
         endpoint_reaped={endpoint_reaped}, remaining={remaining:?}, refusal={observation:?}"
    );

    assert!(repair.is_ok(), "fixture DB restoration failed: {repair:?}");
    assert!(
        owner_released,
        "fixture owner still held after closing its pool"
    );
    assert!(
        endpoint_reaped,
        "independent fixture listener was not reaped"
    );
    assert!(
        remaining.is_empty(),
        "unreaped fixture incarnations: {remaining:?}"
    );
    let refusal =
        observation.unwrap_or_else(|error| panic!("{source:?}/{window:?}/{fault:?}: {error}"));
    let row = refused_row.expect("restored real refusal row");
    assert_same_row(&expected, &row);
    assert_eq!(
        refusal.execs,
        vec![old_pid],
        "no exec was admitted: {refusal:?}"
    );
    let effects = &refusal.endpoint;
    assert!(
        effects.alive
            && effects.no_term
            && effects.same_inode
            && effects.connectable
            && effects.no_requests,
        "refusal acquired endpoint authority: {refusal:?}"
    );
    if matches!(source, Source::Replace) && !cancel {
        assert_eq!(
            refusal.code,
            Some(match fault {
                Fault::Read | Fault::Path => "codex_app_server",
                Fault::Phase => "conflict",
            })
        );
    }
    // These assertions are deliberately after DB repair, automatic recovery and
    // verified child cleanup, so the expected red cannot leak fixture resources.
    assert_eq!(
        refusal.state,
        SharedDaemonState::Failed,
        "{refusal:?}; recovered={recovered:?}"
    );
    assert!(
        !refusal.ready,
        "consumed source remained ready: {refusal:?}"
    );
    assert!(
        refusal.heal,
        "refusal gave no automatic recovery opportunity: {refusal:?}"
    );
    let running = recovered.expect("the already-armed heal must recover after DB repair without ensure_running or schedule_heal");
    assert_eq!(running.state, SharedDaemonState::Running);
    assert_eq!(recovered_status.state, SharedDaemonState::Running);
    assert_eq!(
        running.pid,
        recovered_status.runtime.map(|runtime| runtime.pid)
    );
    assert_ne!(running.pid, Some(old_pid));
    assert!(
        recovered_birth.is_some(),
        "recovered PID had no real native identity"
    );
    assert_eq!(running.process_start_time, recovered_birth);
    assert!(recovered_ready);
    assert_eq!(execs_after, vec![old_pid, running.pid.unwrap()]);
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn replace_second_read_refusal_terminalizes_and_heals() {
    let _env = ENV_LOCK.lock().await;
    post_consumption(Source::Replace, Window::StartBody, Fault::Read, false).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn owned_crash_second_read_refusal_terminalizes_and_heals() {
    let _env = ENV_LOCK.lock().await;
    post_consumption(Source::OwnedCrash, Window::StartBody, Fault::Read, false).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn adopted_crash_second_read_refusal_terminalizes_and_heals() {
    let _env = ENV_LOCK.lock().await;
    post_consumption(Source::AdoptedCrash, Window::StartBody, Fault::Read, false).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn replace_detached_read_refusal_terminalizes_and_heals() {
    let _env = ENV_LOCK.lock().await;
    post_consumption(Source::Replace, Window::Detached, Fault::Read, false).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn cancelled_replace_detached_read_refusal_terminalizes_and_heals() {
    let _env = ENV_LOCK.lock().await;
    post_consumption(Source::Replace, Window::Detached, Fault::Read, true).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn replace_second_phase_refusal_terminalizes_without_effects() {
    let _env = ENV_LOCK.lock().await;
    post_consumption(Source::Replace, Window::StartBody, Fault::Phase, false).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn replace_detached_phase_refusal_terminalizes_without_effects() {
    let _env = ENV_LOCK.lock().await;
    post_consumption(Source::Replace, Window::Detached, Fault::Phase, false).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn replace_second_path_refusal_terminalizes_and_heals() {
    let _env = ENV_LOCK.lock().await;
    post_consumption(Source::Replace, Window::StartBody, Fault::Path, false).await;
}

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn replace_detached_path_refusal_terminalizes_and_heals() {
    let _env = ENV_LOCK.lock().await;
    post_consumption(Source::Replace, Window::Detached, Fault::Path, false).await;
}
