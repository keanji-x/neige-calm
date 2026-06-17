use std::sync::Arc;

use dashmap::DashMap;

use crate::harness::SpecHarness;
use crate::session_projection_repo::RuntimeId;

#[derive(Clone, Default)]
pub struct HarnessRegistry(Arc<DashMap<RuntimeId, SpecHarness>>);

impl HarnessRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, runtime_id: RuntimeId, handle: SpecHarness) -> Option<SpecHarness> {
        self.0.insert(runtime_id, handle)
    }

    pub fn get(&self, runtime_id: &RuntimeId) -> Option<SpecHarness> {
        self.0.get(runtime_id).map(|entry| entry.value().clone())
    }

    pub fn remove(&self, runtime_id: &RuntimeId) -> Option<SpecHarness> {
        self.0.remove(runtime_id).map(|(_, handle)| handle)
    }

    /// Issue #682 review — remove and return every registered harness so
    /// the replay binary's `POST /dev/reset` can shut them down before
    /// reseeding (see `replay::shutdown_registered_harnesses`). Without
    /// this, each dev-forced harness survives a reset as an orphaned
    /// 50ms-tick task whose snapshot persists warn forever against the
    /// reseeded (runtime-row-less) repo. Fixtures-gated: production code
    /// only ever removes harnesses one at a time via [`Self::remove`].
    #[cfg(feature = "fixtures")]
    pub fn drain_all_for_dev(&self) -> Vec<SpecHarness> {
        let runtime_ids: Vec<RuntimeId> = self.0.iter().map(|entry| entry.key().clone()).collect();
        runtime_ids
            .iter()
            .filter_map(|runtime_id| self.remove(runtime_id))
            .collect()
    }

    pub fn len_active(&self) -> usize {
        self.0.len()
    }
}
