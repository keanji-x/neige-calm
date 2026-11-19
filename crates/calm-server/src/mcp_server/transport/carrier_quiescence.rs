//! Per-worker tool admission through the handler and transport reply flush.
use super::RpcError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

#[derive(Default)]
struct State {
    calls: usize,
    fenced: Option<Arc<()>>,
    idle_waiter: Option<oneshot::Sender<ToolCallGuard>>,
}
impl State {
    fn close_is_waiting(&self) -> bool {
        self.idle_waiter
            .as_ref()
            .is_some_and(|waiter| !waiter.is_closed())
    }
}
#[derive(Clone, Default)]
pub struct ToolCallAdmission(Arc<Mutex<HashMap<String, State>>>);

tokio::task_local! {
    static REPLY_CALLS: std::cell::RefCell<Vec<ToolCallGuard>>;
}

/// The transport owns admitted calls until their result has been flushed to the caller.
pub(super) async fn through_reply<F: std::future::Future>(reply: F) -> F::Output {
    REPLY_CALLS.scope(Default::default(), reply).await
}

pub(crate) enum IdleFence {
    Acquired(ToolCallGuard),
    Waiting(oneshot::Receiver<ToolCallGuard>),
    AlreadyWaiting,
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
        self.try_acquire_locked(worker, rotation, state)
            .ok_or_else(|| RpcError::conflict("carrier tool admission is fenced"))
    }

    fn try_acquire_locked(
        &self,
        worker: &str,
        rotation: bool,
        state: &mut State,
    ) -> Option<ToolCallGuard> {
        if state.fenced.is_some() || (rotation && state.calls != 0) || state.close_is_waiting() {
            return None;
        }
        let identity = Arc::new(());
        if rotation {
            state.fenced = Some(identity.clone());
        } else {
            state.calls += 1;
        }
        Some(ToolCallGuard {
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

    /// A committed close can wait for this session without holding up the rest of the sweep.
    pub(crate) fn fence_when_idle(&self, worker: &str) -> IdleFence {
        let mut entries = self.0.lock().expect("tool admission");
        let state = entries.entry(worker.into()).or_default();
        if let Some(guard) = self.try_acquire_locked(worker, true, state) {
            return IdleFence::Acquired(guard);
        }
        if state.close_is_waiting() {
            return IdleFence::AlreadyWaiting;
        }
        let (sender, receiver) = oneshot::channel();
        state.idle_waiter = Some(sender);
        IdleFence::Waiting(receiver)
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
        if state.fenced.is_some() || state.calls != 0 {
            return;
        }
        let Some(waiter) = state.idle_waiter.take() else {
            entries.remove(&self.worker);
            return;
        };
        // Reserve the fence under the same lock as the final release. A dropped receiver
        // returns the guard to us, so release the map lock before sending it.
        let guard = self
            .owner
            .try_acquire_locked(&self.worker, true, state)
            .expect("idle session");
        drop(entries);
        let _ = waiter.send(guard);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn committed_close_drains_admitted_calls_without_accepting_later_calls() {
        let admission = ToolCallAdmission::default();
        let first = admission.enter("closing").unwrap();
        let second = admission.enter("closing").unwrap();
        let IdleFence::Waiting(mut idle) = admission.fence_when_idle("closing") else {
            panic!("both original replies remain in flight");
        };
        assert!(
            admission.enter("closing").is_err(),
            "later calls cannot extend a committed close's drain"
        );
        let other = admission.enter("other").unwrap();
        drop(first);
        assert!(matches!(
            idle.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        drop(second);
        let fence = idle.await.unwrap();
        fence.require_idle().unwrap();
        assert!(admission.enter("closing").is_err());
        drop(fence);
        drop(other);
        assert_eq!(admission.entry_count(), 0);
    }

    #[tokio::test]
    async fn abandoned_close_waiter_does_not_block_calls_or_the_next_drain() {
        let admission = ToolCallAdmission::default();
        let first = admission.enter("closing").unwrap();
        let IdleFence::Waiting(idle) = admission.fence_when_idle("closing") else {
            panic!("original reply remains in flight");
        };
        drop(idle);
        let second = admission.enter("closing").unwrap();
        let IdleFence::Waiting(idle) = admission.fence_when_idle("closing") else {
            panic!("the next cleanup must own its own drain");
        };
        drop(first);
        drop(second);
        let fence = idle.await.unwrap();
        fence.require_idle().unwrap();
        drop(fence);
        assert_eq!(admission.entry_count(), 0);
    }

    #[tokio::test]
    async fn reply_write_flush_error_and_cancellation_release_the_original_fence() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for outcome in ["flush", "error", "cancel"] {
            let admission = ToolCallAdmission::default();
            let calls = admission.clone();
            let (mut read, mut write) = tokio::io::duplex(1);
            let (entered, ready) = oneshot::channel();
            let writer = tokio::spawn(through_reply(async move {
                assert!(calls.enter("closing").unwrap().hold_until_reply().is_none());
                entered.send(()).unwrap();
                write.write_all(b"complete reply").await?;
                write.flush().await
            }));
            ready.await.unwrap();
            let IdleFence::Waiting(mut idle) = admission.fence_when_idle("closing") else {
                panic!("a reply still in the write path must retain its call");
            };
            assert!(matches!(
                idle.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            match outcome {
                "flush" => {
                    let mut bytes = Vec::new();
                    read.read_to_end(&mut bytes).await.unwrap();
                    assert_eq!(bytes, b"complete reply");
                    writer.await.unwrap().unwrap();
                }
                "error" => {
                    drop(read);
                    assert!(writer.await.unwrap().is_err());
                }
                "cancel" => {
                    writer.abort();
                    assert!(writer.await.unwrap_err().is_cancelled());
                }
                _ => unreachable!(),
            }
            let fence = idle.await.unwrap();
            fence.require_idle().unwrap();
            let IdleFence::Waiting(next) = admission.fence_when_idle("closing") else {
                panic!("a second close must wait for the original rotation fence");
            };
            drop(fence);
            let fence = next.await.unwrap();
            fence.require_idle().unwrap();
            drop(fence);
            assert_eq!(admission.entry_count(), 0);
        }
    }
}
