use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::harness::SpecHarness;
use crate::session_projection_repo::RuntimeId;

/// #953 §5 — registry-local monotonic reservation identity. Minted by a
/// checked increment (panic on exhaustion — the clean anti-ABA invariant;
/// unreachable in practice at u64 width), never reused, so a stale
/// [`HarnessReservation`] guard can be recognized by id equality alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservationId(u64);

/// #953 §5 — a registry slot is either a claim in flight (`Reserved`) or an
/// installed live harness (`Live`). `Reserved` slots are exclusively owned
/// by their [`HarnessReservation`] guard: [`HarnessRegistry::remove`] is
/// Live-only and no-ops on them, and only the guard carrying the matching
/// id may install into or release the slot.
pub enum Slot {
    Reserved(ReservationId),
    Live(SpecHarness),
}

struct RegistryInner {
    map: DashMap<RuntimeId, Slot>,
    next_reservation: AtomicU64,
}

#[derive(Clone)]
pub struct HarnessRegistry(Arc<RegistryInner>);

impl Default for HarnessRegistry {
    fn default() -> Self {
        Self(Arc::new(RegistryInner {
            map: DashMap::new(),
            next_reservation: AtomicU64::new(0),
        }))
    }
}

/// #953 §5 — RAII claim on a registry slot. Obtained from
/// [`HarnessRegistry::try_reserve`] / [`HarnessRegistry::reserve_replacing`];
/// consumed by [`Self::install`]. `Drop` without install releases the slot
/// (spawn-failure release). Both install and Drop mutate the slot ONLY if it
/// still holds `Reserved` with this guard's id — a guard superseded by a
/// later `reserve_replacing` (or whose slot was removed and re-claimed) is
/// inert and can never stomp a newer claim.
pub struct HarnessReservation {
    registry: HarnessRegistry,
    runtime_id: RuntimeId,
    id: ReservationId,
    /// Set by `install` so the consuming call suppresses the Drop release.
    done: bool,
}

impl HarnessReservation {
    /// Swap the slot to `Live(handle)` iff it still holds `Reserved(self.id)`.
    /// Returns `false` (inert no-op) when the slot was removed, superseded,
    /// or re-reserved — the caller MUST then shut down the handle it just
    /// built instead of leaking its run loop.
    ///
    /// Entry-guard mechanics (deadlock-free): match `entry(runtime_id)`; on
    /// `Occupied`, compare against `Reserved(self.id)` and mutate through the
    /// occupied entry's own `insert` — never `DashMap::remove` while holding
    /// the entry guard.
    #[must_use = "a false install means the caller must shut down the handle it built"]
    pub fn install(mut self, handle: SpecHarness) -> bool {
        self.done = true;
        match self.registry.0.map.entry(self.runtime_id.clone()) {
            Entry::Occupied(mut occupied) => {
                if matches!(occupied.get(), Slot::Reserved(id) if *id == self.id) {
                    occupied.insert(Slot::Live(handle));
                    true
                } else {
                    false
                }
            }
            Entry::Vacant(_) => false,
        }
    }

    /// Fixtures/test-only: forge a second guard with the same id, so a test
    /// can withhold a stale guard past its owner's Drop (#953 test 14 iii).
    #[cfg(any(test, feature = "fixtures"))]
    pub fn duplicate_for_test(&self) -> HarnessReservation {
        HarnessReservation {
            registry: self.registry.clone(),
            runtime_id: self.runtime_id.clone(),
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
        if let Entry::Occupied(occupied) = self.registry.0.map.entry(self.runtime_id.clone())
            && matches!(occupied.get(), Slot::Reserved(id) if *id == self.id)
        {
            // Release through the occupied entry's own remove — never
            // `DashMap::remove` while holding the entry guard.
            occupied.remove();
        }
    }
}

impl HarnessRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_reservation_id(&self) -> ReservationId {
        let id = self
            .0
            .next_reservation
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_add(1))
            .expect("harness reservation id space exhausted (u64)");
        ReservationId(id)
    }

    /// Direct Live install (test seams). Stomps whatever occupies the slot
    /// (a superseded reservation's guard becomes inert — same posture as
    /// [`Self::reserve_replacing`]); returns the previous Live handle.
    /// Production registration paths go through reserve → install.
    pub fn insert(&self, runtime_id: RuntimeId, handle: SpecHarness) -> Option<SpecHarness> {
        match self.0.map.insert(runtime_id, Slot::Live(handle)) {
            Some(Slot::Live(previous)) => Some(previous),
            _ => None,
        }
    }

    /// #953 §5 — claim a vacant slot. Single `entry()` op: vacant ⇒ insert
    /// `Reserved(fresh_id)` and return the guard; occupied (Reserved OR
    /// Live) ⇒ `None`. Deferred recovery's `SkipIfClaimed` claim.
    pub fn try_reserve(&self, runtime_id: RuntimeId) -> Option<HarnessReservation> {
        let id = self.next_reservation_id();
        match self.0.map.entry(runtime_id.clone()) {
            Entry::Occupied(_) => None,
            Entry::Vacant(vacant) => {
                vacant.insert(Slot::Reserved(id));
                Some(HarnessReservation {
                    registry: self.clone(),
                    runtime_id,
                    id,
                    done: false,
                })
            }
        }
    }

    /// #953 §5 — atomic swap to `Reserved(fresh_id)` regardless of the prior
    /// slot, returning the previous Live handle so the caller can shut it
    /// down outside the map lock. Supersede semantics: a prior reservation's
    /// guard becomes inert (its id no longer matches). Boot recovery / user
    /// resume / start-adapter replace path.
    pub fn reserve_replacing(
        &self,
        runtime_id: RuntimeId,
    ) -> (HarnessReservation, Option<SpecHarness>) {
        let id = self.next_reservation_id();
        let previous_live = match self.0.map.entry(runtime_id.clone()) {
            Entry::Occupied(mut occupied) => match occupied.insert(Slot::Reserved(id)) {
                Slot::Live(handle) => Some(handle),
                Slot::Reserved(_) => None,
            },
            Entry::Vacant(vacant) => {
                vacant.insert(Slot::Reserved(id));
                None
            }
        };
        (
            HarnessReservation {
                registry: self.clone(),
                runtime_id,
                id,
                done: false,
            },
            previous_live,
        )
    }

    pub fn get(&self, runtime_id: &RuntimeId) -> Option<SpecHarness> {
        self.0
            .map
            .get(runtime_id)
            .and_then(|entry| match entry.value() {
                Slot::Live(handle) => Some(handle.clone()),
                Slot::Reserved(_) => None,
            })
    }

    /// #953 §5 — removes **Live entries only**; no-ops on `Reserved` (a
    /// reservation is exclusively owned by its guard — Drop is the owner's
    /// cancel, id-checked). All four Live-targeting production call sites
    /// (user shutdown, wave shutdown, old-runtime supersede, start
    /// compensation) keep these semantics.
    pub fn remove(&self, runtime_id: &RuntimeId) -> Option<SpecHarness> {
        match self.0.map.entry(runtime_id.clone()) {
            Entry::Occupied(occupied) => match occupied.get() {
                Slot::Live(_) => match occupied.remove() {
                    Slot::Live(handle) => Some(handle),
                    Slot::Reserved(_) => unreachable!("checked Live under the entry guard"),
                },
                Slot::Reserved(_) => None,
            },
            Entry::Vacant(_) => None,
        }
    }

    /// Issue #682 review — remove and return every registered Live harness
    /// so the replay binary's `POST /dev/reset` can shut them down before
    /// reseeding (see `replay::shutdown_registered_harnesses`). Without
    /// this, each dev-forced harness survives a reset as an orphaned
    /// 50ms-tick task whose snapshot persists warn forever against the
    /// reseeded (runtime-row-less) repo. Fixtures-gated: production code
    /// only ever removes harnesses one at a time via [`Self::remove`].
    /// Reserved slots stay untouched (their guards own them).
    #[cfg(feature = "fixtures")]
    pub fn drain_all_for_dev(&self) -> Vec<SpecHarness> {
        let runtime_ids: Vec<RuntimeId> =
            self.0.map.iter().map(|entry| entry.key().clone()).collect();
        runtime_ids
            .iter()
            .filter_map(|runtime_id| self.remove(runtime_id))
            .collect()
    }

    /// Number of installed Live harnesses (in-flight reservations excluded).
    pub fn len_active(&self) -> usize {
        self.0
            .map
            .iter()
            .filter(|entry| matches!(entry.value(), Slot::Live(_)))
            .count()
    }
}

// #953 test 14 — stale reservation guards. All four production stale-guard
// orderings are deterministic registry-level state machines; the
// install-failure shutdown path (14 iv) lives in `harness::tests` where a
// real handle's run loop can be asserted shut down.
#[cfg(test)]
mod tests {
    use super::*;

    use crate::harness::snapshot::HarnessSnapshot;
    use crate::harness::{HarnessConfig, SpecHarnessParams};
    use crate::ids::{CardId, WaveId};
    use crate::shared_codex_appserver::SharedCodexAppServer;
    use std::sync::Arc;

    async fn unstarted_handle(runtime_id: &str) -> SpecHarness {
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        let daemon = SharedCodexAppServer::new_stub(repo.clone());
        let (handle, _obs_rx) = SpecHarness::run_unstarted_for_test(
            SpecHarnessParams {
                runtime_id: runtime_id.to_string(),
                wave_id: WaveId::from("wave-registry-test".to_string()),
                card_id: CardId::from("card-registry-test".to_string()),
                thread_id: None,
                repo,
                events: crate::event::EventBus::new(),
                card_role_cache: crate::card_role_cache::CardRoleCache::new(),
                wave_cove_cache: crate::wave_cove_cache::WaveCoveCache::new(),
                daemon,
                config: HarnessConfig::default(),
                snapshot: HarnessSnapshot::initial(0, vec![]),
            },
            4,
        );
        handle
    }

    /// 14(i) — `remove()` no-ops on Reserved: the slot stays claimed (a
    /// second `try_reserve` still loses), and only the owner guard's Drop
    /// releases it.
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

    /// 14(ii) — `reserve_replacing` supersedes an in-flight reservation:
    /// the stale guard's install returns false (newer claim untouched) and
    /// its Drop is inert.
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

    /// 14(iii) — remove-and-reclaim: a withheld stale guard (leaked via the
    /// test duplicate hook) from a released reservation mutates nothing
    /// against the re-claimed slot.
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

    /// #953 test 8 (reservation release) — a forced spawn failure drops the
    /// guard without install: the slot is vacant again and a user resume can
    /// claim it.
    #[tokio::test]
    async fn dropped_reservation_releases_slot_for_user_claim() {
        let registry = HarnessRegistry::new();
        let runtime_id = "rt-release".to_string();
        {
            let _reservation = registry.try_reserve(runtime_id.clone()).expect("vacant");
            // Spawn failed: guard dropped without install.
        }
        assert!(registry.get(&runtime_id).is_none());
        let (reservation, previous_live) = registry.reserve_replacing(runtime_id.clone());
        assert!(previous_live.is_none());
        let handle = unstarted_handle(&runtime_id).await;
        assert!(reservation.install(handle));
        assert_eq!(registry.len_active(), 1);
    }

    /// Live-only accounting: `get`/`len_active` exclude in-flight
    /// reservations; `reserve_replacing` over a Live slot returns the old
    /// handle for shutdown outside the map lock.
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
