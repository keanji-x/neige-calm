//! Identity of an owner-local park. Admission is checked under the owner's lock.
use std::sync::{Arc, Mutex, Weak};

#[derive(Default)]
pub(crate) struct ParkState(Mutex<Weak<()>>);
pub(crate) struct ParkToken<'a> {
    owner: &'a ParkState,
    identity: Arc<()>,
}
impl ParkState {
    pub(crate) fn is_parked(&self) -> bool {
        self.0.lock().expect("carrier park").upgrade().is_some()
    }
    pub(crate) fn park(&self) -> Option<ParkToken<'_>> {
        let mut state = self.0.lock().expect("carrier park");
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
            .0
            .lock()
            .expect("carrier park")
            .ptr_eq(&Arc::downgrade(&self.identity))
    }
}
// Dropping the token expires only its own Weak identity. It cannot clear a newer park.
