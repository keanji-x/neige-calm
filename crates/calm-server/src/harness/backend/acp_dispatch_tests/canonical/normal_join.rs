//! A genuine prior join still requires boot-owned absence proof before recovery.
use super::*;
#[tokio::test]
async fn canonical_acp_genuine_join_still_requires_boot_absence_proof() {
    let fx = Fixture::with_storage("lost", true).await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    let before = fx.receipt_bytes(&fx.submission).await;
    fx.session.shutdown().await.unwrap();
    let (backend, session, _held) = session_for(&fx.owner, &fx.dir);
    backend.recover_retained().await.unwrap();
    assert!(backend.settle_kept_execution().await.unwrap().is_none());
    fx.keep_unknown(&fx.submission, "explicit-cold-keep").await;
    assert!(matches!(
        session.restore_normal_join_after_keep().await,
        Err(crate::error::CalmError::Conflict(_))
    ));
    assert!(backend.settle_kept_execution().await.unwrap().is_none());
    assert!(backend.settle_kept_execution().await.unwrap().is_none());
    assert_eq!(fx.receipt_bytes(&fx.submission).await, before);
    assert!(
        crate::db::sqlite::acp_submission_unresolved(fx.owner.repo.pool(), &fx.owner.worker)
            .await
            .unwrap()
    );
    assert!(
        crate::harness::input_dispatch::complete_once(
            fx.owner.repo.as_ref(),
            &backend,
            &fx.owner.card,
            &fx.owner.worker
        )
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(fx.prompts(), 1);
    session.shutdown().await.unwrap();
}
