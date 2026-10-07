//! Public checkpoint writers must serialize with the turn-issuance owner.
use super::completed_commit_tests::Fixture;
use std::time::Duration;

#[tokio::test]
async fn public_snapshot_persistence_waits_for_issuance_checkpoint_owner() {
    let fixture = Fixture::new().await;
    let issuance = fixture.harness.inner.issuance.lock().await;
    let save = fixture.harness.persist_snapshot();
    tokio::pin!(save);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), save.as_mut())
            .await
            .is_err(),
        "an external writer must not overwrite the issuance checkpoint"
    );
    drop(issuance);
    tokio::time::timeout(Duration::from_secs(2), save)
        .await
        .unwrap()
        .unwrap();
}
