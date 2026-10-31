//! The requests a running turn is paused on, owned by its harness table.
//! Only the actor inserts; the authorized answer owner or a lifecycle sweep takes
//! entries. Mutation publishes a scheduling hint after releasing the table lock.
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

pub use provider::held_requests::{
    ConnectionId, HeldRequestMessage, HeldRequestSender, HeldResponder, RequestKey,
};
pub(crate) use provider::held_requests::{HeldRequestReceiver, channel};

struct HeldEntry {
    request_key: RequestKey,
    connection: ConnectionId,
    responder: Box<dyn HeldResponder>,
}
#[derive(Default)]
struct State {
    entries: HashMap<i64, HeldEntry>,
    since: Option<Instant>,
    elapsed: Duration,
}
/// Lifetime total includes an ongoing hold. Sampling cannot lose a hold which
/// opens and closes between actor passes, and never counts an overlap twice.
pub(crate) struct HeldTime {
    pub total: Duration,
    pub active: bool,
}
#[derive(Default)]
pub struct HeldRequests {
    state: StdMutex<State>,
    changed: Arc<Notify>,
}
impl HeldRequests {
    pub(crate) fn with_signal(changed: Arc<Notify>) -> Self {
        Self {
            state: StdMutex::default(),
            changed,
        }
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("planner harness held requests mutex poisoned")
    }
    fn change<T>(&self, change: impl FnOnce(&mut HashMap<i64, HeldEntry>) -> (T, bool)) -> T {
        let mut state = self.lock();
        let (result, changed) = change(&mut state.entries);
        let now = Instant::now();
        if state.entries.is_empty() {
            if let Some(since) = state.since.take() {
                state.elapsed += now.saturating_duration_since(since);
            }
        } else if state.since.is_none() {
            state.since = Some(now);
        }
        drop(state);
        if changed {
            self.changed.notify_one();
        }
        result
    }
    pub(crate) fn insert(
        &self,
        ask_id: i64,
        request_key: RequestKey,
        connection: ConnectionId,
        responder: Box<dyn HeldResponder>,
    ) {
        let previous = self.change(|entries| {
            (
                entries.insert(
                    ask_id,
                    HeldEntry {
                        request_key,
                        connection,
                        responder,
                    },
                ),
                true,
            )
        });
        drop(previous);
    }
    pub fn contains(&self, ask_id: i64) -> bool {
        self.lock().entries.contains_key(&ask_id)
    }
    pub fn is_empty(&self) -> bool {
        self.lock().entries.is_empty()
    }
    pub fn ask_ids(&self) -> std::collections::HashSet<i64> {
        self.lock().entries.keys().copied().collect()
    }
    pub fn take(&self, ask_id: i64) -> Option<Box<dyn HeldResponder>> {
        self.change(|entries| {
            let entry = entries.remove(&ask_id);
            let changed = entry.is_some();
            (entry, changed)
        })
        .map(|entry| entry.responder)
    }
    fn take_where(&self, matches: impl Fn(&HeldEntry) -> bool) -> Vec<Box<dyn HeldResponder>> {
        self.change(|entries| {
            let ids: Vec<_> = entries
                .iter()
                .filter(|(_, entry)| matches(entry))
                .map(|(id, _)| *id)
                .collect();
            let taken: Vec<_> = ids
                .into_iter()
                .filter_map(|id| entries.remove(&id))
                .collect();
            let changed = !taken.is_empty();
            (taken, changed)
        })
        .into_iter()
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
    pub(crate) fn time(&self) -> HeldTime {
        let state = self.lock();
        HeldTime {
            total: state.elapsed
                + state
                    .since
                    .map(|since| Instant::now().saturating_duration_since(since))
                    .unwrap_or_default(),
            active: state.since.is_some(),
        }
    }
    #[cfg(feature = "fixtures")]
    pub(crate) fn rewind_active_for_test(&self, by: Duration) {
        let mut state = self.lock();
        if let Some(since) = state.since.as_mut() {
            *since = since
                .checked_sub(by)
                .expect("representable actual hold rewind");
        }
    }
}
