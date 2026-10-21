//! Exact accepted-input sources belong to the original worker, even while its
//! native slot is dormant. SQL owns ordering and every actual execution gate.
use crate::conversation_inputs::InputSource;
use calm_types::conversation::CanonicalPendingIdentity;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub(crate) struct CanonicalSourceIndex {
    sources: Mutex<HashMap<CanonicalPendingIdentity, InputSource>>,
}
impl CanonicalSourceIndex {
    pub(crate) fn retire(&self) {
        self.sources.lock().expect("canonical source index").clear();
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.sources
            .lock()
            .expect("canonical source index")
            .is_empty()
    }
    pub(crate) fn observe(&self, input: CanonicalPendingIdentity, source: InputSource) {
        let source = source.for_identity(&input);
        self.sources
            .lock()
            .expect("canonical source index")
            .insert(input, source);
    }
    pub(crate) fn source(&self, input: &CanonicalPendingIdentity) -> Option<InputSource> {
        self.sources
            .lock()
            .expect("canonical source index")
            .get(input)
            .cloned()
    }
    /// Only exact references returned from committed consumption are removed.
    /// An older runtime projection must never prune a concurrently accepted tail.
    pub(crate) fn consumed(&self, inputs: &[CanonicalPendingIdentity]) {
        let mut sources = self.sources.lock().expect("canonical source index");
        for input in inputs {
            sources.remove(input);
        }
    }
}
