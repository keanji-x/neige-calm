use super::SharedOwnerRegistry;
use calm_session::terminal_session::InputPermission;
use futures::future::BoxFuture;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, OwnedMutexGuard};

#[derive(Clone)]
pub enum ClientInputScope {
    InteractiveUser,
    Bound {
        observe: Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>,
        control: Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>,
    },
}
impl ClientInputScope {
    pub async fn allowed(&self) -> bool {
        match self {
            Self::InteractiveUser => true,
            Self::Bound { observe: check, .. } => {
                tokio::time::timeout(std::time::Duration::from_secs(5), check())
                    .await
                    .unwrap_or(false)
            }
        }
    }
    pub async fn control_allowed(&self) -> bool {
        match self {
            Self::InteractiveUser => true,
            Self::Bound { control, .. } => {
                tokio::time::timeout(std::time::Duration::from_secs(5), control())
                    .await
                    .unwrap_or(false)
            }
        }
    }
}

#[derive(Default)]
pub struct InputBarrier {
    serial: Arc<Mutex<()>>,
    write_completion: Arc<Mutex<()>>,
    uncertain: AtomicBool,
}
impl InputBarrier {
    /// Wait for an admitted connection write's outcome to be enqueued before forwarding exit.
    /// Pending ownership claims do not hold this fence; waiting grants no input authority.
    pub async fn wait_for_write_completion(&self) {
        drop(self.write_completion.lock().await);
    }

    /// Test seam: hold every grant (handshakes, claims and the writer's admission) until the
    /// guard drops, so a test can change state while a write waits in the writer's queue.
    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub async fn hold_for_test(&self) -> OwnedMutexGuard<()> {
        self.serial.clone().lock_owned().await
    }

    pub async fn grant(&self) -> Option<OwnedMutexGuard<()>> {
        let guard = self.serial.clone().lock_owned().await;
        if self.uncertain.load(Ordering::Acquire) {
            None
        } else {
            Some(guard)
        }
    }
}

#[derive(Clone)]
pub enum WriteAuthority {
    TrustedKernel,
    Connection {
        permission: InputPermission,
        registry: SharedOwnerRegistry,
        barrier: Arc<InputBarrier>,
        scope: ClientInputScope,
    },
}
impl WriteAuthority {
    pub async fn admit(&self) -> Option<WriteGuard> {
        match self {
            Self::TrustedKernel => Some(WriteGuard {
                ownership: None,
                _serial: None,
                _write_completion: None,
                started: false,
                completed: false,
            }),
            Self::Connection {
                permission,
                registry,
                barrier,
                scope,
            } => {
                let serial = barrier.grant().await?;
                if !scope.control_allowed().await {
                    return None;
                }
                let allowed = match permission {
                    InputPermission::Owner(lease) => registry
                        .lock()
                        .is_ok_and(|registry| registry.is_current(*lease)),
                    InputPermission::Kernel => true,
                    InputPermission::Denied => false,
                };
                if !allowed {
                    return None;
                }
                let completion = barrier.write_completion.clone().lock_owned().await;
                Some(WriteGuard {
                    ownership: Some(barrier.clone()),
                    _serial: Some(serial),
                    _write_completion: Some(completion),
                    started: false,
                    completed: false,
                })
            }
        }
    }
}

pub struct WriteGuard {
    ownership: Option<Arc<InputBarrier>>,
    _serial: Option<OwnedMutexGuard<()>>,
    _write_completion: Option<OwnedMutexGuard<()>>,
    started: bool,
    completed: bool,
}
impl WriteGuard {
    pub fn started(&mut self) {
        self.started = true;
    }
    pub fn completed(&mut self) {
        self.completed = true;
    }
}
impl Drop for WriteGuard {
    fn drop(&mut self) {
        // Lost acknowledgement is not proof of a stopped write. Prevent a new
        // grant on this renderer after timeout, transport loss or task cancellation.
        if self.started
            && !self.completed
            && let Some(barrier) = &self.ownership
        {
            barrier.uncertain.store(true, Ordering::Release);
        }
    }
}
