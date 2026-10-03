//! Deletion seals: the provider-neutral fence that says "this thread's track or area is being
//! deleted; start no turn and recover no harness on it" (#1981 S3).
//!
//! One [`ThreadSeals`] exists per server. The shared Codex daemon builds it and every other
//! holder (the deletion routes, the harness registry and recovery, a Claude Planner session) is
//! handed the same `Arc` by the state wiring.

use std::sync::Arc;

use dashmap::DashMap;

/// The threads whose owning track or area is being deleted.
pub struct ThreadSeals {
    sealed: DashMap<String, ()>,
}

impl ThreadSeals {
    /// Only the shared Codex daemon's constructors call this; everyone else shares its `Arc`.
    pub(crate) fn new() -> Self {
        Self {
            sealed: DashMap::new(),
        }
    }

    /// Set before the owner is quiesced: from here on no turn starts on `thread_id`.
    pub fn seal_for_deletion(&self, thread_id: &str) {
        self.sealed.insert(thread_id.to_string(), ());
    }

    /// The deletion did not happen; the thread may run turns again.
    pub fn unseal_after_rollback(&self, thread_id: &str) {
        self.sealed.remove(thread_id);
    }

    pub fn is_sealed(&self, thread_id: &str) -> bool {
        self.sealed.contains_key(thread_id)
    }

    /// The deletion committed: drop the seals of its threads. Returns how many were sealed.
    pub fn forget_deleted(&self, thread_ids: &[String]) -> usize {
        thread_ids
            .iter()
            .filter(|thread_id| self.sealed.remove(thread_id.as_str()).is_some())
            .count()
    }
}

/// Owns deletion-time thread seals; dropping an unfinished step rolls every seal back, and
/// `retain` transfers the quiesced ids to the transaction half of the deletion saga.
pub(crate) struct DeletionThreadSeals {
    seals: Arc<ThreadSeals>,
    thread_ids: Vec<String>,
    rollback_on_drop: bool,
}

impl DeletionThreadSeals {
    pub(crate) fn new(seals: Arc<ThreadSeals>) -> Self {
        Self {
            seals,
            thread_ids: Vec::new(),
            rollback_on_drop: true,
        }
    }

    pub(crate) fn seal(&mut self, thread_id: impl Into<String>) {
        let thread_id = thread_id.into();
        if self
            .thread_ids
            .iter()
            .any(|existing| existing == &thread_id)
        {
            return;
        }
        self.seals.seal_for_deletion(&thread_id);
        self.thread_ids.push(thread_id);
    }

    pub(crate) fn retain(mut self) -> Vec<String> {
        self.thread_ids.sort();
        self.thread_ids.dedup();
        self.rollback_on_drop = false;
        std::mem::take(&mut self.thread_ids)
    }
}

impl Drop for DeletionThreadSeals {
    fn drop(&mut self) {
        if self.rollback_on_drop {
            for thread_id in &self.thread_ids {
                self.seals.unseal_after_rollback(thread_id);
            }
        }
    }
}
