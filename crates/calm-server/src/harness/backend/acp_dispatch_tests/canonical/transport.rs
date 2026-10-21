//! All native write failures after receipt insertion remain fenced, regardless of byte count.
#![cfg(test)]
use super::*;
use crate::acp_planner::test_transport::{self, Fault};
#[tokio::test]
async fn canonical_acp_partial_flush_and_write_timeout_hold_receipt_without_resend() {
    for fault in [Fault::Partial, Fault::Flush, Fault::Timeout] {
        let fx = Fixture::new("reply").await;
        let probe = test_transport::arm(&fx.owner.worker, fault);
        let report = fx.dispatch().await.unwrap();
        assert!(
            matches!(report.report.delivery(), InputDelivery::Unknown(_)),
            "{fault:?}"
        );
        fx.wait_idle().await;
        assert!(
            !probe.bytes.lock().unwrap().is_empty(),
            "must cross actual writer"
        );
        let state: String = sqlx::query_scalar("SELECT state FROM acp_submissions")
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
        assert_eq!(state, "unknown");
        assert!(fx.complete().await.is_none());
        let before = probe.bytes.lock().unwrap().clone();
        assert!(fx.dispatch().await.is_err());
        assert_eq!(*probe.bytes.lock().unwrap(), before);
        assert_eq!(fx.prompts(), 0);
        fx.session.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn canonical_acp_cancellation_during_partial_native_write_retains_driver() {
    let fx = Arc::new(Fixture::new("reply").await);
    let probe = test_transport::arm(&fx.owner.worker, Fault::Timeout);
    let dispatch = tokio::spawn({
        let fx = fx.clone();
        async move { fx.dispatch().await }
    });
    tokio::time::timeout(Duration::from_secs(20), probe.crossed.notified())
        .await
        .unwrap();
    dispatch.abort();
    match dispatch.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("dispatch survived cancellation"),
    }
    fx.session.shutdown().await.unwrap();
    let before = probe.bytes.lock().unwrap().clone();
    assert!(!before.is_empty());
    assert!(fx.complete().await.is_none());
    assert!(fx.dispatch().await.is_err());
    assert_eq!(*probe.bytes.lock().unwrap(), before);
    let state: String = sqlx::query_scalar("SELECT state FROM acp_submissions")
        .fetch_one(fx.owner.repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "unknown");
}
