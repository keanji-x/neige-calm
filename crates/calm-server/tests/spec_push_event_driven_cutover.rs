//! #335 PR2 - event-driven spec app-server cutover coverage.
//!
//! These are process-level tests against the fake `codex app-server`
//! fixture. They keep Option Y's per-wave `codex app-server --listen
//! unix://<sock>` shape while proving the initial turn lifecycle is decided
//! by notifications / EOF / child exit, not by a `turn/started` timer.

#![cfg(unix)]

use std::time::{Duration, Instant};

use calm_server::spec_appserver::{
    SpecPushPhase, TurnWatchdogConfig, spawn_spec_appserver,
    spawn_spec_appserver_with_watchdog_config,
};
use serde_json::json;

fn fake_codex_bin() -> String {
    env!("CARGO_BIN_EXE_osc-probe-child").to_string()
}

async fn wait_for_phase(
    handle: &calm_server::spec_appserver::SpecPushHandle,
    phase: SpecPushPhase,
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if handle.status().await.phase == phase {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for phase {phase:?}; status={:?}",
            handle.status().await
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_path(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if path.exists() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {} to exist",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn initial_turn_completed_without_started_succeeds() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("appserver").join("card-slow").join("sock");
    let env = json!({
        "FAKE_CODEX_SKIP_TURN_STARTED": "1",
        "FAKE_CODEX_TURN_COMPLETED_DELAY_MS": "150"
    });

    let started = Instant::now();
    let handle = spawn_spec_appserver(&fake_codex_bin(), &env, "goal", &sock)
        .await
        .expect("completed lifecycle should satisfy boot");

    assert!(
        started.elapsed() >= Duration::from_millis(100),
        "fake completed delay should have been observed"
    );
    assert_eq!(handle.status().await.phase, SpecPushPhase::TurnCompleted);
    drop(handle);
}

#[tokio::test]
async fn initial_turn_started_then_completed_updates_status() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp
        .path()
        .join("appserver")
        .join("card-normal")
        .join("sock");
    let env = json!({
        "FAKE_CODEX_TURN_COMPLETED_DELAY_MS": "25"
    });

    let handle = spawn_spec_appserver(&fake_codex_bin(), &env, "goal", &sock)
        .await
        .expect("started lifecycle should satisfy boot");

    wait_for_phase(&handle, SpecPushPhase::TurnCompleted).await;
    drop(handle);
}

#[tokio::test]
async fn slow_initial_turn_started_does_not_false_fail() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp
        .path()
        .join("appserver")
        .join("card-slow-start")
        .join("sock");
    let env = json!({
        "FAKE_CODEX_TURN_STARTED_DELAY_MS": "3500"
    });

    let started = Instant::now();
    let handle = spawn_spec_appserver(&fake_codex_bin(), &env, "goal", &sock)
        .await
        .expect("slow but progressing turn/started should satisfy boot");
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(3200),
        "boot returned before the fake turn/started delay elapsed: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "slow turn/started should not wait for a boot budget to elapse: {elapsed:?}"
    );
    assert_eq!(handle.status().await.phase, SpecPushPhase::TurnRunning);
    drop(handle);
}

#[tokio::test]
async fn initial_turn_child_exit_fails_promptly() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("appserver").join("card-exit").join("sock");
    let env = json!({
        "FAKE_CODEX_EXIT_AFTER_TURN_ACK": "1"
    });

    let started = Instant::now();
    let err = match spawn_spec_appserver(&fake_codex_bin(), &env, "goal", &sock).await {
        Ok(handle) => {
            drop(handle);
            panic!("child exit before lifecycle should fail");
        }
        Err(err) => err,
    };

    assert!(
        started.elapsed() < Duration::from_secs(2),
        "child exit should win promptly, got {err}"
    );
    assert!(
        err.to_string().contains("initial turn lifecycle")
            || err.to_string().contains("notification stream closed"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn runtime_watchdog_interrupts_silent_running_turn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp
        .path()
        .join("appserver")
        .join("card-watchdog")
        .join("sock");
    let marker = tmp.path().join("interrupt.marker");
    let env = json!({
        "FAKE_CODEX_INTERRUPT_MARKER": marker.display().to_string()
    });

    let handle = spawn_spec_appserver_with_watchdog_config(
        &fake_codex_bin(),
        &env,
        "goal",
        &sock,
        TurnWatchdogConfig {
            max_turn_duration: Duration::from_millis(100),
            interrupt_completion_budget: Duration::from_secs(2),
        },
    )
    .await
    .expect("initial turn/started should satisfy boot");

    wait_for_phase(&handle, SpecPushPhase::TurnCompleted).await;
    assert!(
        marker.exists(),
        "fake app-server never received turn/interrupt"
    );
    drop(handle);
}

#[tokio::test]
async fn runtime_watchdog_bails_when_interrupt_has_no_completed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp
        .path()
        .join("appserver")
        .join("card-watchdog-no-completed")
        .join("sock");
    let marker = tmp.path().join("interrupt.marker");
    let env = json!({
        "FAKE_CODEX_INTERRUPT_MARKER": marker.display().to_string(),
        "FAKE_CODEX_INTERRUPT_NO_COMPLETED": "1"
    });
    let max_turn_duration = Duration::from_millis(50);
    let interrupt_completion_budget = Duration::from_millis(150);

    let handle = spawn_spec_appserver_with_watchdog_config(
        &fake_codex_bin(),
        &env,
        "goal",
        &sock,
        TurnWatchdogConfig {
            max_turn_duration,
            interrupt_completion_budget,
        },
    )
    .await
    .expect("initial turn/started should satisfy boot");

    let started = Instant::now();
    tokio::time::timeout(
        max_turn_duration + interrupt_completion_budget + Duration::from_secs(1),
        async {
            wait_for_path(&marker).await;
            tokio::time::sleep(interrupt_completion_budget + Duration::from_millis(75)).await;
        },
    )
    .await
    .expect("watchdog no-completed path should be bounded by the interrupt budget");

    assert!(
        started.elapsed()
            < max_turn_duration + interrupt_completion_budget + Duration::from_secs(1),
        "watchdog no-completed path exceeded the bounded test budget"
    );
    // Layer B (#347): watchdog bail now marks the public phase Wedged so
    // later observations do not enqueue into a dead queue. Production
    // handles also signal the runtime recovery supervisor; this direct
    // handle test has no supervisor wired, so it only asserts the phase
    // transition.
    assert_eq!(handle.status().await.phase, SpecPushPhase::Wedged);
    drop(handle);
}

#[tokio::test]
async fn runtime_watchdog_accepts_delayed_interrupted_completion() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp
        .path()
        .join("appserver")
        .join("card-watchdog-delayed-interrupted")
        .join("sock");
    let marker = tmp.path().join("interrupt.marker");
    let env = json!({
        "FAKE_CODEX_INTERRUPT_MARKER": marker.display().to_string(),
        "FAKE_CODEX_INTERRUPT_COMPLETED_DELAY_MS": "120"
    });

    let handle = spawn_spec_appserver_with_watchdog_config(
        &fake_codex_bin(),
        &env,
        "goal",
        &sock,
        TurnWatchdogConfig {
            max_turn_duration: Duration::from_millis(50),
            interrupt_completion_budget: Duration::from_millis(500),
        },
    )
    .await
    .expect("initial turn/started should satisfy boot");

    wait_for_path(&marker).await;
    wait_for_phase(&handle, SpecPushPhase::TurnCompleted).await;
    drop(handle);
}
