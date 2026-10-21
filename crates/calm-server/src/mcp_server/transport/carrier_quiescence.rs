//! Per-worker tool admission, from resolved call identity until handler completion.
use super::RpcError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    calls: usize,
    fenced: Option<Arc<()>>,
}
#[derive(Clone, Default)]
pub struct ToolCallAdmission(Arc<Mutex<HashMap<String, State>>>);

pub struct ToolCallGuard {
    owner: ToolCallAdmission,
    worker: String,
    rotation: bool,
    identity: Arc<()>,
}
impl ToolCallAdmission {
    #[cfg(test)]
    pub(crate) fn entry_count(&self) -> usize {
        self.0.lock().expect("tool admission").len()
    }

    fn acquire(&self, worker: &str, rotation: bool) -> Result<ToolCallGuard, RpcError> {
        let mut entries = self.0.lock().expect("tool admission");
        let state = entries.entry(worker.into()).or_default();
        if state.fenced.is_some() || (rotation && state.calls != 0) {
            return Err(RpcError::conflict("carrier tool admission is fenced"));
        }
        let identity = Arc::new(());
        if rotation {
            state.fenced = Some(identity.clone());
        } else {
            state.calls += 1;
        }
        Ok(ToolCallGuard {
            owner: self.clone(),
            worker: worker.into(),
            rotation,
            identity,
        })
    }
    pub(super) fn enter(&self, worker: &str) -> Result<ToolCallGuard, RpcError> {
        self.acquire(worker, false)
    }
    /// Park only an idle worker through commit/publication; new calls fail.
    pub fn fence(&self, worker: &str) -> Result<ToolCallGuard, RpcError> {
        self.acquire(worker, true)
    }
}
impl ToolCallGuard {
    pub fn require_idle(&self) -> Result<(), RpcError> {
        let entries = self.owner.0.lock().expect("tool admission");
        let state = &entries[&self.worker];
        if !self.rotation
            || state.calls != 0
            || !state
                .fenced
                .as_ref()
                .is_some_and(|id| Arc::ptr_eq(id, &self.identity))
        {
            return Err(RpcError::conflict("carrier has an in-flight MCP tool call"));
        }
        Ok(())
    }
}
impl Drop for ToolCallGuard {
    fn drop(&mut self) {
        let mut entries = self.owner.0.lock().expect("tool admission");
        let state = entries.get_mut(&self.worker).expect("held admission");
        if self.rotation {
            if state
                .fenced
                .as_ref()
                .is_some_and(|id| Arc::ptr_eq(id, &self.identity))
            {
                state.fenced = None;
            }
        } else {
            state.calls -= 1;
        }
        if state.fenced.is_none() && state.calls == 0 {
            entries.remove(&self.worker);
        }
    }
}
