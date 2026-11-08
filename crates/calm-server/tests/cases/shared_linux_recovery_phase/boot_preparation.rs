//! Cold cache preparation must succeed before process recovery can own completion.
use super::*;

#[tokio::test]
#[cfg_attr(not(target_os = "linux"), ignore = "requires the real Linux consumer")]
async fn boot_cache_read_failure_does_not_heal_without_rebuilding_threads() {
    let _env = ENV_LOCK.lock().await;
    let mut fixture = Fixture::new(false, "default").await;
    fixture
        .config
        .shared_codex_appserver_restart_initial_delay_ms = 1;
    fixture.config.shared_codex_appserver_restart_max_delay_ms = 200;
    fixture
        .write(fixture.update(RecoveryWrite::Legacy, SharedDaemonState::Idle))
        .await;
    let card = seed_card(&fixture.repo, 0).await;
    let thread = "cold-preparation-retained-thread";
    seed_runtime_thread(&fixture.repo, &card, thread).await;
    let capture = fixture.root.path().join("boot-requests.ndjson");
    unsafe { std::env::set_var("FAKE_CODEX_CAPTURE_REQUESTS", &capture) };
    let _capture_guard = EnvGuard("FAKE_CODEX_CAPTURE_REQUESTS");
    let daemon = fixture.daemon().await;
    let state = cold_start::boot_state(&fixture, daemon.clone());
    let before = fixture.row().await;
    let mut writer = sqlx::SqliteConnection::connect(&fixture.url).await.unwrap();
    // Keep active worker sessions readable; break the real second cache read.
    sqlx::query("ALTER TABLE worker_session_backends RENAME TO paused_bindings")
        .execute(&mut writer)
        .await
        .unwrap();
    let mut serial = None;
    let observation: Result<_, String> = async {
        let attribution =
            calm_server::session_projection_lookup::merge_active_shared_thread_attribution(
                fixture.repo.as_ref(),
            )
            .await
            .map_err(|error| error.to_string())?;
        if attribution.get(&card).map(String::as_str) != Some(thread) {
            return Err("seeded thread is absent from real cold attribution".into());
        }
        if calm_truth::db::sqlite::session_backend::live_bound_threads(
            fixture.repo.db().read_pool(),
            "codex",
        )
        .await
        .is_ok()
        {
            return Err("cache preparation read unexpectedly succeeded".into());
        }
        fixture
            .repo
            .shared_daemon_runtime_get("default")
            .await
            .map_err(|error| error.to_string())?;
        let boot =
            tokio::time::timeout(Duration::from_secs(5), calm_server::boot_harnesses(&state))
                .await
                .map_err(|_| "boot did not return from cache preparation".to_owned())?
                .map_err(|error| error.to_string())?;
        serial = Some(
            tokio::time::timeout(
                Duration::from_secs(5),
                daemon.lock_transition_serial_for_test(),
            )
            .await
            .map_err(|_| "boot did not release transition serial".to_owned())?,
        );
        Ok((
            boot,
            daemon.status_snapshot().state,
            daemon.heal_active_for_test(),
        ))
    }
    .await;

    // Repair first, release the serial, observe only automatic work, then reap
    // every child before any assertion about the regression (including red).
    let repair = sqlx::query("ALTER TABLE paused_bindings RENAME TO worker_session_backends")
        .execute(&mut writer)
        .await;
    drop(serial.take());
    let automatic = tokio::time::timeout(Duration::from_secs(2), async {
        fixture.exec.capture_new_exec(0).await;
        loop {
            if daemon.status_snapshot().state == SharedDaemonState::Running
                && !daemon.heal_active_for_test()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let status = daemon.status_snapshot();
    let ready = *daemon.readiness_receiver().borrow();
    let heal = daemon.heal_active_for_test();
    let mapping = daemon.cached_card_for_thread(thread);
    let requests = optional_text(&capture)
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>();
    let after = fixture.repo.shared_daemon_runtime_get("default").await;
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
        "cache preparation: repair={repair:?}, remaining={remaining:?}, observation={observation:?}, \
         automatic={automatic:?}, status={status:?}, ready={ready:?}, heal={heal}, \
         mapping={mapping:?}, execs={execs:?}, requests={requests:?}"
    );
    assert!(repair.is_ok(), "restore cache schema: {repair:?}");
    assert!(remaining.is_empty(), "unreaped children: {remaining:?}");
    let (boot, refused, armed) = observation.expect("real cache preparation fault");
    assert_eq!(boot, 0);
    assert_eq!(refused, SharedDaemonState::Idle);
    assert!(
        !armed,
        "incomplete cache preparation armed process-only heal"
    );
    assert!(
        automatic.is_err(),
        "boot claimed recovery without a rebuilt cache"
    );
    assert_eq!(status.state, SharedDaemonState::Idle);
    assert!(!ready.running && !heal);
    assert!(execs.is_empty());
    assert_eq!(mapping, None);
    assert_eq!(
        resume_count(&requests.expect("captured boot RPCs"), thread),
        0
    );
    assert_same_row(&before, &after.expect("restored runtime row"));
}
