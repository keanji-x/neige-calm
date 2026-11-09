//! Per-worker tool admission through the handler and transport reply flush.
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

tokio::task_local! {
    static REPLY_CALLS: std::cell::RefCell<Vec<ToolCallGuard>>;
}

/// The transport owns admitted calls until their result has been flushed to the caller.
pub(super) async fn through_reply<F: std::future::Future>(reply: F) -> (F::Output, bool) {
    let (result, calls) = REPLY_CALLS
        .scope(Default::default(), async {
            let result = reply.await;
            let calls = REPLY_CALLS.with(|calls| calls.take());
            (result, calls)
        })
        .await;
    let admitted = !calls.is_empty();
    drop(calls);
    (result, admitted)
}

pub struct ToolCallGuard {
    owner: ToolCallAdmission,
    worker: String,
    rotation: bool,
    identity: Arc<()>,
}
impl ToolCallAdmission {
    #[cfg(all(test, target_os = "linux", feature = "fixtures"))]
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
    pub(super) fn hold_until_reply(self) -> Option<Self> {
        let mut call = Some(self);
        let _ = REPLY_CALLS
            .try_with(|calls| calls.borrow_mut().push(call.take().expect("admitted call")));
        call
    }
    pub fn worker(&self) -> &str {
        &self.worker
    }
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
