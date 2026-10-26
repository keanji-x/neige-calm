//! Resume uses the live scheduler, not a direct provider admission.
use super::*;
pub(super) struct Pause(crate::test_seams::PausePoint);
impl Pause {
    pub(super) fn arm(point: &str, key: &str) -> Self {
        let pause = crate::test_seams::PausePoint::default();
        crate::test_seams::install_pause_for_test(point, key, pause.clone());
        Self(pause)
    }
    pub(super) async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(20), self.0.entered.notified())
            .await
            .unwrap();
    }
}
impl Drop for Pause {
    fn drop(&mut self) {
        self.0.release.notify_one();
    }
}
#[tokio::test]
async fn c5d_claude_resume_holds_fence_and_dispatches_once() {
    let fx = Fixture::new("exit").await;
    let locks = crate::per_card_lock::KeyedLocks::default();
    let before = Pause::arm("c5d-before-occupancy-commit", &fx.owner.worker);
    let harness = fx
        .start_loop_with_locks(fx.registry.clone(), locks.clone())
        .await;
    before.entered().await;
    assert!(crate::per_card_lock::try_lock_key(&locks, &fx.owner.track).is_none());
    drop(before);
    fx.phase("turn_completed").await;
    for _ in 0..10 {
        harness.refresh_canonical_input().await.unwrap();
    }
    assert_eq!(fx.session.user_line_writes_for_test(), 1);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workspace_executions WHERE track_id=?")
            .bind(&fx.owner.track)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
    harness.shutdown().await.unwrap();
}
