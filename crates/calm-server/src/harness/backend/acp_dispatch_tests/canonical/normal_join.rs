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

#[tokio::test]
async fn canonical_acp_unchanged_normal_join_and_kept_cleanup_do_not_need_writer() {
    let fx = Fixture::new("lost").await;
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    // A genuine join must be recorded even before any Human Keep.
    assert!(fx.backend.settle_kept_execution().await.unwrap().is_none());
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM acp_normal_joins WHERE worker_session_id=?1")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.db().read_pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
    let mut blocked = Vec::new();
    for kept in [false, true] {
        if kept {
            fx.keep_unknown(&fx.submission, "keep-after-genuine-join")
                .await;
            assert!(fx.backend.settle_kept_execution().await.unwrap().is_some());
        }
        let writer = begin_immediate_tx(fx.owner.repo.db()).await.unwrap();
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            fx.backend.settle_kept_execution(),
        )
        .await;
        writer.rollback().await.unwrap();
        match result {
            Ok(result) => assert!(result.unwrap().is_none()),
            Err(_) => blocked.push(kept),
        }
    }
    fx.session.shutdown().await.unwrap();
    assert!(
        blocked.is_empty(),
        "unchanged ACP observations waited for writer: kept={blocked:?}"
    );
}
