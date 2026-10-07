//! The provider requests a Planner's running turn is paused on (#2348), one table per harness.
//!
//! A provider adapter pushes [`HeldRequestMessage`]s into the harness's one ordered, unbounded
//! channel; the run loop is its only consumer, so an `Open`, a sweep and a turn end happen one
//! after another. An `Open` becomes a `hold` ask and an entry in [`HeldRequests`], which owns the
//! request's [`HeldResponder`] from then on. Whoever takes an entry out answers it: the answer
//! route with the chosen option, the run loop by dropping it, which the adapter turns into a
//! refusal. The kernel reads no provider field: a request is an opaque key on an opaque
//! connection. The messages and the responder are the provider layer's contract
//! ([`provider::held_requests`]).

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

pub use provider::held_requests::{
    ConnectionId, HeldRequestMessage, HeldRequestSender, HeldResponder, RequestKey,
};
pub(crate) use provider::held_requests::{HeldRequestReceiver, channel};

struct HeldEntry {
    request_key: RequestKey,
    connection: ConnectionId,
    responder: Box<dyn HeldResponder>,
}

/// The paused requests of one harness by the id of the `hold` ask each one raised. Only the run
/// loop inserts; an entry leaves exactly once, by [`HeldRequests::take`] or a `take_*` sweep.
#[derive(Default)]
pub struct HeldRequests {
    entries: StdMutex<HashMap<i64, HeldEntry>>,
}

impl HeldRequests {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i64, HeldEntry>> {
        self.entries
            .lock()
            .expect("planner harness held requests mutex poisoned")
    }

    pub(crate) fn insert(
        &self,
        ask_id: i64,
        request_key: RequestKey,
        connection: ConnectionId,
        responder: Box<dyn HeldResponder>,
    ) {
        self.lock().insert(
            ask_id,
            HeldEntry {
                request_key,
                connection,
                responder,
            },
        );
    }

    /// Whether the request of `ask_id` is still held.
    pub fn contains(&self, ask_id: i64) -> bool {
        self.lock().contains_key(&ask_id)
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// The asks whose requests are held.
    pub fn ask_ids(&self) -> std::collections::HashSet<i64> {
        self.lock().keys().copied().collect()
    }

    /// Take the request of `ask_id` out to answer it.
    pub fn take(&self, ask_id: i64) -> Option<Box<dyn HeldResponder>> {
        self.lock().remove(&ask_id).map(|entry| entry.responder)
    }

    fn take_where(&self, matches: impl Fn(&HeldEntry) -> bool) -> Vec<Box<dyn HeldResponder>> {
        let mut entries = self.lock();
        let ask_ids: Vec<i64> = entries
            .iter()
            .filter(|(_, entry)| matches(entry))
            .map(|(ask_id, _)| *ask_id)
            .collect();
        ask_ids
            .into_iter()
            .filter_map(|ask_id| entries.remove(&ask_id))
            .map(|entry| entry.responder)
            .collect()
    }

    pub(crate) fn take_request(&self, request_key: &RequestKey) -> Vec<Box<dyn HeldResponder>> {
        self.take_where(|entry| &entry.request_key == request_key)
    }

    pub(crate) fn take_connection(&self, connection: &ConnectionId) -> Vec<Box<dyn HeldResponder>> {
        self.take_where(|entry| &entry.connection == connection)
    }

    pub(crate) fn take_all(&self) -> Vec<Box<dyn HeldResponder>> {
        self.take_where(|_| true)
    }
}
