//! Current-frame observation must not wait behind unrelated SQLite writers.
use super::*;
use crate::db::sqlite::{begin_immediate_tx, session_set_handle_state_tx};
use crate::harness::run_loop::completed_commit_tests::Fixture;

async fn fixture() -> Fixture {
    let fx = Fixture::new().await;
    let inner = &fx.harness.inner;
    let mut snapshot = CanonicalHarnessSnapshot::dormant(0);
    snapshot.header.last_thread_id = inner.thread_id.read().await.clone();
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    assert!(
        session_set_handle_state_tx(
            &mut tx,
            &inner.worker_session_id,
            Some(serde_json::to_value(snapshot).unwrap()),
        )
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();
    fx
}

#[tokio::test]
async fn canonical_observation_does_not_acquire_the_writer() {
    let fx = fixture().await;
    let inner = &fx.harness.inner;
    let before = read_current(inner).await.unwrap();
    let writer = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let observed = tokio::time::timeout(Duration::from_secs(1), read_current(inner)).await;
    writer.rollback().await.unwrap();
    let observed = observed
        .expect("a read-only observation must not wait for the writer")
        .unwrap();
    assert_eq!(observed.snapshot, before.snapshot);
    assert_eq!(observed.fence, before.fence);
}

#[tokio::test]
async fn canonical_observation_cannot_commit_after_carrier_replacement() {
    let fx = fixture().await;
    let inner = &fx.harness.inner;
    let observed = read_current(inner).await.unwrap();
    sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?")
        .bind(inner.card_id.as_str())
        .execute(fx.repo.db().writer_pool())
        .await
        .unwrap();
    assert!(matches!(
        read_current(inner).await,
        Err(CalmError::Conflict(_))
    ));
    let mut tx = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let result = persist_runtime_frame_tx(
        &mut tx,
        inner.card_id.as_str(),
        &inner.worker_session_id,
        &observed.fence,
        header_value(&observed.snapshot).unwrap(),
    )
    .await;
    tx.rollback().await.unwrap();
    assert!(matches!(
        result.map_err(CalmError::from),
        Err(CalmError::Conflict(_))
    ));
}

#[tokio::test]
async fn canonical_observation_of_no_completed_execution_does_not_acquire_the_writer() {
    let fx = fixture().await;
    let inner = &fx.harness.inner;
    let writer = begin_immediate_tx(fx.repo.db()).await.unwrap();
    let observed = tokio::time::timeout(
        Duration::from_secs(1),
        crate::harness::input_dispatch::complete_once(
            fx.repo.as_ref(),
            &inner.backend,
            inner.card_id.as_str(),
            &inner.worker_session_id,
        ),
    )
    .await;
    writer.rollback().await.unwrap();
    assert!(
        observed
            .expect("an empty END observation must not wait for the writer")
            .unwrap()
            .is_none()
    );
}
