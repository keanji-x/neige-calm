use std::sync::Arc;

use dashmap::DashMap;

use crate::harness::SpecHarness;
use crate::runtime_repo::RuntimeId;

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

    pub fn len_active(&self) -> usize {
        self.0.len()
    }
}
