use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::harness::PlannerHarness;
use crate::harness::live_replies::LiveReplies;
use crate::ids::TrackId;

mod sources;
pub(crate) use sources::CanonicalSourceIndex;

/// Registry-local monotonic reservation identity, never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservationId(u64);

/// Native ownership may sleep while the original worker and its input sources remain.
pub enum Slot {
    Dormant,
    Reserved(ReservationId),
    Live(PlannerHarness),
}

struct WorkerEntry {
    slot: Slot,
    track: Option<TrackId>,
    sources: Arc<CanonicalSourceIndex>,
}
impl WorkerEntry {
    fn new(slot: Slot) -> Self {
        Self {
            slot,
            track: None,
            sources: Arc::new(CanonicalSourceIndex::default()),
        }
    }
}

struct RegistryInner {
    map: DashMap<String, WorkerEntry>,
    next_reservation: AtomicU64,
    live_replies: Arc<LiveReplies>,
}

#[derive(Clone)]
pub struct HarnessRegistry(Arc<RegistryInner>);

impl Default for HarnessRegistry {
    fn default() -> Self {
        Self(Arc::new(RegistryInner {
            map: DashMap::new(),
            next_reservation: AtomicU64::new(0),
            live_replies: Arc::new(LiveReplies::new()),
        }))
    }
}

/// An exclusive native installation claim. Cancelling it leaves the same original
/// worker dormant; it cannot discard input sources accepted during restoration.
pub struct HarnessReservation {
    registry: HarnessRegistry,
    worker_session_id: String,
    id: ReservationId,
    done: bool,
}

impl HarnessReservation {
    #[must_use = "a false install means the caller must shut down the handle it built"]
    pub fn install(mut self, handle: PlannerHarness) -> bool {
        self.done = true;
        match self.registry.0.map.entry(self.worker_session_id.clone()) {
            Entry::Occupied(mut occupied) => {
                if matches!(occupied.get().slot, Slot::Reserved(id) if id == self.id) {
                    handle.mark_installed();
                    occupied.get_mut().track = Some(handle.track_id().clone());
                    occupied.get_mut().slot = Slot::Live(handle);
                    true
                } else {
                    false
                }
            }
            Entry::Vacant(_) => false,
        }
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn duplicate_for_test(&self) -> HarnessReservation {
        HarnessReservation {
            registry: self.registry.clone(),
            worker_session_id: self.worker_session_id.clone(),
            id: self.id,
            done: false,
        }
    }
}

impl Drop for HarnessReservation {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        if let Entry::Occupied(mut occupied) =
            self.registry.0.map.entry(self.worker_session_id.clone())
            && matches!(occupied.get().slot, Slot::Reserved(id) if id == self.id)
        {
            if occupied.get().track.is_none() && occupied.get().sources.is_empty() {
                // A cancelled pre-install historical claim has no accepted
                // canonical association and no original bound worker to retain.
                occupied.remove();
            } else {
                occupied.get_mut().slot = Slot::Dormant;
            }
        }
    }
}

impl HarnessRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The sole identity index belongs to the worker entry, independently of its
    /// native slot. The loop borrows this same Arc and never retains the registry.
    pub(crate) fn canonical_sources(
        &self,
        worker: &str,
        track: &TrackId,
    ) -> Arc<CanonicalSourceIndex> {
        let mut entry = self
            .0
            .map
            .entry(worker.to_owned())
            .or_insert_with(|| WorkerEntry::new(Slot::Dormant));
        entry.track = Some(track.clone());
        entry.sources.clone()
    }

    pub async fn reserve_after_shutdown(
        &self,
        worker_session_id: String,
    ) -> crate::error::Result<HarnessReservation> {
        loop {
            let previous = self.get(&worker_session_id);
            if let Some(previous) = previous.as_ref() {
                previous.shutdown().await?;
            }
            let id = self.next_reservation_id();
            match self.0.map.entry(worker_session_id.clone()) {
                Entry::Occupied(mut occupied) => {
                    if let Slot::Live(current) = &occupied.get().slot
                        && !previous
                            .as_ref()
                            .is_some_and(|previous| current.same_instance(previous))
                    {
                        continue;
                    }
                    occupied.get_mut().slot = Slot::Reserved(id);
                }
                Entry::Vacant(vacant) => {
                    vacant.insert(WorkerEntry::new(Slot::Reserved(id)));
                }
            }
            return Ok(HarnessReservation {
                registry: self.clone(),
                worker_session_id,
                id,
                done: false,
            });
        }
    }

    /// Explicit record retirement ends both native ownership and its input-source
    /// associations. Ordinary hibernation must use remove_if_same instead.
    pub async fn shutdown_and_remove(
        &self,
        worker_session_id: &str,
    ) -> crate::error::Result<Option<PlannerHarness>> {
        let mut stopped = None;
        loop {
            let Some(handle) = self.get(&worker_session_id.to_owned()) else {
                if let Entry::Occupied(occupied) = self.0.map.entry(worker_session_id.to_owned())
                    && matches!(occupied.get().slot, Slot::Dormant)
                {
                    occupied.get().sources.retire();
                    occupied.remove();
                }
                return Ok(stopped);
            };
            handle.shutdown().await?;
            let removed = self.retire_if_same(worker_session_id, &handle);
            stopped = Some(handle);
            if removed {
                return Ok(stopped);
            }
        }
    }

    /// Detach only the confirmed predecessor's native slot. The original record
    /// and its exact-input source associations survive hibernation and recovery.
    pub(crate) fn remove_if_same(&self, worker: &str, expected: &PlannerHarness) -> bool {
        if let Entry::Occupied(mut occupied) = self.0.map.entry(worker.to_owned())
            && matches!(&occupied.get().slot, Slot::Live(current) if current.same_instance(expected))
        {
            occupied.get_mut().slot = Slot::Dormant;
            return true;
        }
        false
    }

    /// Consume an already retired record after its exact native owner stopped.
    /// This is distinct from detaching a sleeping or recoverable original worker.
    pub(crate) fn retire_if_same(&self, worker: &str, expected: &PlannerHarness) -> bool {
        if let Entry::Occupied(occupied) = self.0.map.entry(worker.to_owned())
            && matches!(&occupied.get().slot, Slot::Live(current) if current.same_instance(expected))
        {
            occupied.get().sources.retire();
            occupied.remove();
            return true;
        }
        false
    }

    pub fn live_replies(&self) -> &Arc<LiveReplies> {
        &self.0.live_replies
    }

    fn next_reservation_id(&self) -> ReservationId {
        let id = self
            .0
            .next_reservation
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_add(1))
            .expect("harness reservation id space exhausted (u64)");
        ReservationId(id)
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn insert(&self, runtime_id: String, handle: PlannerHarness) -> Option<PlannerHarness> {
        handle.mark_installed();
        let mut entry = self
            .0
            .map
            .entry(runtime_id)
            .or_insert_with(|| WorkerEntry::new(Slot::Dormant));
        entry.track = Some(handle.track_id().clone());
        match std::mem::replace(&mut entry.slot, Slot::Live(handle)) {
            Slot::Live(previous) => Some(previous),
            Slot::Dormant | Slot::Reserved(_) => None,
        }
    }

    /// Claim native ownership without replacing the worker entry or its sources.
    pub fn try_reserve(&self, runtime_id: String) -> Option<HarnessReservation> {
        let id = self.next_reservation_id();
        let mut entry = self
            .0
            .map
            .entry(runtime_id.clone())
            .or_insert_with(|| WorkerEntry::new(Slot::Dormant));
        if !matches!(entry.slot, Slot::Dormant) {
            return None;
        }
        entry.slot = Slot::Reserved(id);
        Some(HarnessReservation {
            registry: self.clone(),
            worker_session_id: runtime_id,
            id,
            done: false,
        })
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn reserve_replacing(
        &self,
        runtime_id: String,
    ) -> (HarnessReservation, Option<PlannerHarness>) {
        let id = self.next_reservation_id();
        let mut entry = self
            .0
            .map
            .entry(runtime_id.clone())
            .or_insert_with(|| WorkerEntry::new(Slot::Dormant));
        let previous = match std::mem::replace(&mut entry.slot, Slot::Reserved(id)) {
            Slot::Live(handle) => Some(handle),
            Slot::Dormant | Slot::Reserved(_) => None,
        };
        (
            HarnessReservation {
                registry: self.clone(),
                worker_session_id: runtime_id,
                id,
                done: false,
            },
            previous,
        )
    }

    pub fn get(&self, runtime_id: &String) -> Option<PlannerHarness> {
        self.0
            .map
            .get(runtime_id)
            .and_then(|entry| match &entry.slot {
                Slot::Live(handle) => Some(handle.clone()),
                Slot::Dormant | Slot::Reserved(_) => None,
            })
    }

    /// Fixture detachment mirrors hibernation and preserves the original entry.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn remove(&self, runtime_id: &str) -> Option<PlannerHarness> {
        let mut entry = self.0.map.get_mut(runtime_id)?;
        if !matches!(entry.slot, Slot::Live(_)) {
            return None;
        }
        match std::mem::replace(&mut entry.slot, Slot::Dormant) {
            Slot::Live(handle) => Some(handle),
            _ => unreachable!("checked live under the entry guard"),
        }
    }

    pub fn live_for_track(&self, track_id: &TrackId) -> Vec<(String, PlannerHarness)> {
        self.0
            .map
            .iter()
            .filter_map(|entry| match &entry.slot {
                Slot::Live(handle) if handle.track_id() == track_id => {
                    Some((entry.key().clone(), handle.clone()))
                }
                _ => None,
            })
            .collect()
    }

    pub async fn shutdown_track(
        &self,
        track_id: &TrackId,
        thread_seals: Arc<crate::thread_seals::ThreadSeals>,
    ) -> crate::error::Result<Vec<String>> {
        let handles = self.live_for_track(track_id);
        let mut seals = crate::thread_seals::DeletionThreadSeals::new(thread_seals.clone());
        for (worker, handle) in handles {
            if let Some(thread) = handle.shutdown_for_deletion(thread_seals.clone()).await? {
                seals.seal(thread);
            }
            self.retire_if_same(&worker, &handle);
        }
        self.0.map.retain(|_, entry| {
            let retired =
                entry.track.as_ref() == Some(track_id) && matches!(entry.slot, Slot::Dormant);
            if retired {
                entry.sources.retire();
            }
            !retired
        });
        Ok(seals.retain())
    }

    #[cfg(feature = "fixtures")]
    pub fn drain_all_for_dev(&self) -> Vec<PlannerHarness> {
        let runtime_ids: Vec<String> = self.0.map.iter().map(|entry| entry.key().clone()).collect();
        runtime_ids
            .iter()
            .filter_map(|runtime_id| {
                let Entry::Occupied(occupied) = self.0.map.entry(runtime_id.clone()) else {
                    return None;
                };
                if matches!(occupied.get().slot, Slot::Reserved(_)) {
                    return None;
                }
                occupied.get().sources.retire();
                match occupied.remove().slot {
                    Slot::Live(handle) => Some(handle),
                    Slot::Dormant => None,
                    Slot::Reserved(_) => unreachable!("reservation checked under the entry guard"),
                }
            })
            .collect()
    }

    pub fn len_active(&self) -> usize {
        self.0
            .map
            .iter()
            .filter(|entry| matches!(entry.slot, Slot::Live(_)))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn unstarted_handle(worker_session_id: &str) -> PlannerHarness {
        crate::harness::run_loop::registry_handle_for_test(worker_session_id).await
    }

    #[tokio::test]
    async fn remove_no_ops_on_reserved_slot() {
        let registry = HarnessRegistry::new();
        let runtime_id = "rt-remove-reserved".to_string();
        let reservation = registry.try_reserve(runtime_id.clone()).expect("vacant");
        assert!(registry.remove(&runtime_id).is_none());
        assert!(
            registry.try_reserve(runtime_id.clone()).is_none(),
            "remove() on a Reserved slot must leave the claim intact"
        );
        assert!(registry.get(&runtime_id).is_none());
        drop(reservation);
        assert!(
            registry.try_reserve(runtime_id.clone()).is_some(),
            "owner Drop releases the slot"
        );
    }

    #[tokio::test]
    async fn superseded_reservation_install_and_drop_are_inert() {
        let registry = HarnessRegistry::new();
        let runtime_id = "rt-supersede".to_string();
        let reservation_a = registry.try_reserve(runtime_id.clone()).expect("vacant");
        let stale_a_drop = reservation_a.duplicate_for_test();
        let (reservation_b, previous_live) = registry.reserve_replacing(runtime_id.clone());
        assert!(previous_live.is_none(), "prior slot was Reserved, not Live");

        let handle_a = unstarted_handle(&runtime_id).await;
        assert!(
            !reservation_a.install(handle_a.clone()),
            "stale install must not stomp the newer claim"
        );
        handle_a.shutdown().await.unwrap();
        drop(stale_a_drop);
        assert!(
            registry.try_reserve(runtime_id.clone()).is_none(),
            "stale drop must not release the newer claim"
        );

        let handle_b = unstarted_handle(&runtime_id).await;
        assert!(reservation_b.install(handle_b.clone()));
        let live = registry.get(&runtime_id).expect("B installed");
        live.shutdown().await.unwrap();
    }

    /// A withheld stale guard from a released reservation mutates nothing against the re-claimed slot.
    #[tokio::test]
    async fn stale_guard_after_reclaim_mutates_nothing() {
        let registry = HarnessRegistry::new();
        let runtime_id = "rt-reclaim".to_string();
        let reservation_a = registry.try_reserve(runtime_id.clone()).expect("vacant");
        let stale_a = reservation_a.duplicate_for_test();
        let stale_a_drop = reservation_a.duplicate_for_test();
        drop(reservation_a);

        let reservation_b = registry.try_reserve(runtime_id.clone()).expect("released");
        let handle_a = unstarted_handle(&runtime_id).await;
        assert!(
            !stale_a.install(handle_a.clone()),
            "stale install after reclaim must be inert"
        );
        handle_a.shutdown().await.unwrap();
        drop(stale_a_drop);

        let handle_b = unstarted_handle(&runtime_id).await;
        assert!(reservation_b.install(handle_b.clone()));
        assert!(registry.get(&runtime_id).is_some());
        registry
            .remove(&runtime_id)
            .expect("Live entry removable")
            .shutdown()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn dropped_reservation_releases_slot_for_user_claim() {
        let registry = HarnessRegistry::new();
        let runtime_id = "rt-release".to_string();
        {
            let _reservation = registry.try_reserve(runtime_id.clone()).expect("vacant");
            // Spawn failed: guard dropped without install.
        }
        assert!(registry.get(&runtime_id).is_none());
        assert!(
            !registry.0.map.contains_key(&runtime_id),
            "an unbound source-free cancellation must not accumulate dormant entries"
        );
        let (reservation, previous_live) = registry.reserve_replacing(runtime_id.clone());
        assert!(previous_live.is_none());
        let handle = unstarted_handle(&runtime_id).await;
        assert!(reservation.install(handle));
        assert_eq!(registry.len_active(), 1);
    }

    #[tokio::test]
    async fn reserve_replacing_returns_previous_live_handle() {
        let registry = HarnessRegistry::new();
        let runtime_id = "rt-replace-live".to_string();
        let reservation = registry.try_reserve(runtime_id.clone()).expect("vacant");
        assert_eq!(registry.len_active(), 0);
        let handle = unstarted_handle(&runtime_id).await;
        assert!(reservation.install(handle));
        assert_eq!(registry.len_active(), 1);

        let (reservation2, previous_live) = registry.reserve_replacing(runtime_id.clone());
        let previous_live = previous_live.expect("previous Live handle returned");
        previous_live.shutdown().await.unwrap();
        assert_eq!(registry.len_active(), 0);
        let handle2 = unstarted_handle(&runtime_id).await;
        assert!(reservation2.install(handle2));
        assert_eq!(registry.len_active(), 1);
    }
}
