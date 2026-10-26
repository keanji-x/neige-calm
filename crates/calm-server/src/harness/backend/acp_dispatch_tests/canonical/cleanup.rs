//! Canonical pre-receipt cleanup uses the same production issue path.
#![cfg(test)]
use super::super::cleanup::Failure;
use super::*;
use crate::acp_planner::test_seams::pause_issue;

async fn canonical(failure: Failure) {
    let mut fx = Fixture::new("reply").await;
    if matches!(failure, Failure::SpawnRevoke) {
        let (backend, session, _held) = session_for_command(
            &fx.owner,
            &fx.dir,
            fx.provider.clone(),
            "/missing-acp-executable",
        );
        fx.backend = backend;
        fx.session = session;
    }
    let pause = pause_issue(&fx.owner.worker, failure.stage());
    let selection = failure.selection();
    let handoff = fx.backend.handoff_for_test().await;
    let issue = crate::harness::input_dispatch::initial(
        fx.owner.repo.as_ref(),
        &fx.source,
        &fx.owner.card,
        &fx.owner.worker,
        &fx.submission,
        &fx.backend,
        &selection,
        PlannerPermissionMode::Never,
        &handoff,
    );
    tokio::pin!(issue);
    tokio::select! {
        result = &mut issue => panic!("issue skipped cleanup fault window: {}", result.is_ok()),
        result = tokio::time::timeout(Duration::from_secs(20), pause.entered.notified()) => result.unwrap(),
    }
    failure.arm(&fx.owner).await;
    pause.release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(20), issue)
        .await
        .unwrap();
    failure.clear(&fx.owner).await;
    fx.session.shutdown().await.unwrap();
    let report = result.unwrap();
    assert!(matches!(
        report.report.delivery(),
        InputDelivery::NotDispatched(_)
    ));
    let error = report
        .report
        .outcome()
        .as_ref()
        .expect_err("cleanup failed");
    assert!(
        matches!(error, TurnStartFailure::Transient(error)
        if error.to_string().contains(failure.expected())),
        "{error:?}"
    );
    assert_eq!(fx.prompts(), 0);
    assert!(fx.complete().await.is_none());
}
#[tokio::test]
async fn canonical_acp_cleanup_spawn_revoke() {
    canonical(Failure::SpawnRevoke).await;
}
#[tokio::test]
async fn canonical_acp_cleanup_prepared_revoke() {
    canonical(Failure::PreparedRevoke).await;
}
#[tokio::test]
async fn canonical_acp_cleanup_prepared_stop() {
    canonical(Failure::PreparedStop).await;
}
#[tokio::test]
async fn canonical_acp_cleanup_revoke_precedes_stop() {
    canonical(Failure::PreparedBoth).await;
}
