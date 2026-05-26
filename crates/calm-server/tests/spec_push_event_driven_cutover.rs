//! #335 PR2 - event-driven spec app-server cutover coverage.
//!
//! These are process-level tests against the fake `codex app-server`
//! fixture. They keep Option Y's per-wave `codex app-server --listen
//! unix://<sock>` shape while proving the initial turn lifecycle is decided
//! by notifications / EOF / child exit, not by a `turn/started` timer.

#![cfg(unix)]

use std::time::{Duration, Instant};

use calm_server::spec_appserver::{SpecPushPhase, spawn_spec_appserver};
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
