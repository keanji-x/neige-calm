//! Per-card async lock map.
//!
//! Lifted from the private `PerCardMintLocks` machinery in
//! `operation::spec_harness_start_adapter` (issue #649 i2) so the
//! `/spec/input` lazy-recovery path can serialize per-card work without
//! reaching into the adapter's internals. Guards self-clean their map entry
//! on drop, so an idle card costs nothing.
//!
//! Transient stale entries are possible if a waiter is canceled between
//! strong_count snapshots; same-card locks will reuse a stale entry safely.

use std::sync::Arc;

use dashmap::DashMap;

pub type PerCardLocks = Arc<DashMap<String, Arc<tokio::sync::Mutex<()>>>>;

pub fn new_per_card_locks() -> PerCardLocks {
    Arc::new(DashMap::new())
}

pub struct PerCardLockGuard {
    card_id: String,
    lock: Arc<tokio::sync::Mutex<()>>,
    locks: PerCardLocks,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for PerCardLockGuard {
    fn drop(&mut self) {
        let _ = self.guard.take();
        self.locks.remove_if(&self.card_id, |_, existing| {
            Arc::ptr_eq(existing, &self.lock) && Arc::strong_count(existing) == 2
        });
    }
}

pub async fn lock_card(locks: &PerCardLocks, card_id: &str) -> PerCardLockGuard {
    let lock = locks
        .entry(card_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let guard = lock.clone().lock_owned().await;
    PerCardLockGuard {
        card_id: card_id.to_string(),
        lock,
        locks: locks.clone(),
        guard: Some(guard),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn per_card_locks_block_same_card_only_and_cleanup() {
        let locks = new_per_card_locks();
        let first_a = lock_card(&locks, "card-A").await;

        let locks_for_a = locks.clone();
        let same_card = async move {
            let mut second_a = Box::pin(lock_card(&locks_for_a, "card-A"));
            tokio::select! {
                _guard = &mut second_a => {
                    panic!("same-card lock acquired while the first card-A guard was held");
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }

            drop(first_a);
            let second_guard = tokio::time::timeout(Duration::from_secs(1), &mut second_a)
                .await
                .expect("same-card lock should complete after first guard drops");
            drop(second_guard);
        };

        let locks_for_b = locks.clone();
        let other_card = async move {
            let guard =
                tokio::time::timeout(Duration::from_millis(50), lock_card(&locks_for_b, "card-B"))
                    .await
                    .expect("card-B lock should not wait behind card-A");
            drop(guard);
        };

        tokio::join!(same_card, other_card);
        assert!(
            !locks.contains_key("card-A"),
            "card-A lock entry should be removed once all guards drop"
        );
    }
}
