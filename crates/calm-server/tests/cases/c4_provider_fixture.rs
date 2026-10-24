//! Registered provider fixture teardown, including a still-running production rewind.
use super::claude_planner_session_fixture::{RegisteredRig, wait_for_file};
use calm_truth::db::sqlite::provider_execution as sql;
use provider::control::EditPreparation;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[test]
fn native_macos_registered_provider_fixture_cleanup() {
    c4_registered_fixture_closes_after_completion();
    c4_registered_fixture_closes_after_early_return();
    c4_registered_fixture_closes_after_panic();
}

#[tokio::test]
async fn c4_registered_fixture_closes_after_completion() {
    let rig = RegisteredRig::new("exit").await;
    let cleanup = rig.execution_cleanup_probe();
    let root = rig.dir.path().to_owned();
    assert!(matches!(
        rig.session()
            .prepare_edit_for_test(&rig.thread)
            .await
            .unwrap(),
        EditPreparation::Prepared { .. }
    ));
    let before = Instant::now();
    drop(rig);
    assert!(before.elapsed() < Duration::from_secs(10));
    cleanup.assert_closed();
    assert!(!root.exists(), "private provider root remains");
}

const EARLY_RETURN: &str = "fixture test returned before rewind completed";
const PANIC: &str = "fixture test panicked before rewind completed";

async fn leave_live_rewind(rig: RegisteredRig, panic: bool) -> Result<(), &'static str> {
    std::fs::write(
        rig.bin("claude"),
        "#!/bin/sh\ntouch ready; exec sleep 120\n",
    )
    .unwrap();
    let session = rig.session().clone();
    let thread = rig.thread.clone();
    let completed = calm_server::test_seams::PausePoint::default();
    calm_server::test_seams::install_pause_for_test(
        "c4-release-completed",
        &rig.worker_session_id,
        completed.clone(),
    );
    let mut request = Box::pin(session.prepare_edit_for_test(&thread));
    let ready = rig.dir.path().join("ws/ready");
    tokio::select! {
        result = &mut request => panic!("rewind completed before fixture exit: {result:?}"),
        () = wait_for_file(&ready) => {}
    }
    tokio::select! {
        result = &mut request => panic!("rewind completed before fixture exit: {result:?}"),
        entered = tokio::time::timeout(Duration::from_secs(10), completed.entered.notified()) => entered.unwrap(),
    }
    let claim = sql::load(rig.repo.pool(), &rig.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    completed.release.notify_one();
    assert!(
        matches!(claim.record().release, sql::Release::Released { .. }),
        "release after completed seam: {:?}",
        claim.record().release
    );
    // Keep the request pending while the rig leaves scope. No explicit Stop/sweep is used:
    // the fixture's existing supervisor owner must close the live registered payload.
    let _rig = rig;
    if panic {
        std::panic::panic_any(PANIC);
    }
    Err(EARLY_RETURN)
}

async fn assert_early_cleanup(panic: bool) {
    let rig = RegisteredRig::new("exit").await;
    let cleanup = rig.execution_cleanup_probe();
    let root = rig.dir.path().to_owned();
    let before = Instant::now();
    let result = tokio::spawn(leave_live_rewind(rig, panic)).await;
    // Also check closure when the task failed before reaching the intended exit point.
    cleanup.assert_closed();
    assert!(!root.exists(), "private provider root remains");
    assert!(before.elapsed() < Duration::from_secs(20));
    if panic {
        let error = result.unwrap_err();
        assert!(error.is_panic());
        let payload = error.into_panic();
        assert_eq!(payload.downcast_ref::<&str>().copied(), Some(PANIC));
    } else {
        assert_eq!(result.unwrap(), Err(EARLY_RETURN));
    }
}

#[tokio::test]
async fn c4_registered_fixture_closes_after_early_return() {
    assert_early_cleanup(false).await;
}

#[tokio::test]
async fn c4_registered_fixture_closes_after_panic() {
    assert_early_cleanup(true).await;
}
