//! Identity of an owner-local park. Admission is checked under the owner's lock.
use std::sync::{Arc, Mutex, Weak};

#[derive(Default)]
pub(crate) struct ParkState {
    identity: Mutex<Weak<()>>,
    changed: Arc<tokio::sync::Notify>,
}
pub(crate) struct ParkToken<'a> {
    owner: &'a ParkState,
    identity: Arc<()>,
}
impl ParkState {
    pub(crate) fn with_signal(changed: Arc<tokio::sync::Notify>) -> Self {
        Self {
            identity: Mutex::default(),
            changed,
        }
    }
    pub(crate) fn is_parked(&self) -> bool {
        self.identity
            .lock()
            .expect("carrier park")
            .upgrade()
            .is_some()
    }
    pub(crate) fn park(&self) -> Option<ParkToken<'_>> {
        let mut state = self.identity.lock().expect("carrier park");
        if state.upgrade().is_some() {
            return None;
        }
        let identity = Arc::new(());
        *state = Arc::downgrade(&identity);
        Some(ParkToken {
            owner: self,
            identity,
        })
    }
}
impl ParkToken<'_> {
    pub(crate) fn is_current(&self) -> bool {
        self.owner
            .identity
            .lock()
            .expect("carrier park")
            .ptr_eq(&Arc::downgrade(&self.identity))
    }
}
// Only this token's identity can be released, then the owner may re-examine work.
impl Drop for ParkToken<'_> {
    fn drop(&mut self) {
        let mut current = self.owner.identity.lock().expect("carrier park");
        if current.ptr_eq(&Arc::downgrade(&self.identity)) {
            *current = Weak::new();
            drop(current);
            self.owner.changed.notify_one();
        }
    }
}
