//! Forge owns one observer per operation in this process. No durable ownership lives here.
use super::SpawnCtx;
use std::collections::{HashMap, hash_map::Entry};
use std::sync::{Mutex, OnceLock};

fn registry() -> &'static Mutex<HashMap<String, bool>> {
    static OBSERVERS: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    OBSERVERS.get_or_init(Default::default)
}

/// Registration precedes spawning. The guard is captured by the task even before its first poll.
pub(super) fn ensure_observer(ctx: SpawnCtx, id: String) -> Option<tokio::task::JoinHandle<()>> {
    let mut observers = registry().lock().unwrap();
    match observers.entry(id.clone()) {
        Entry::Occupied(mut entry) => {
            *entry.get_mut() = true;
            None
        }
        Entry::Vacant(entry) => {
            entry.insert(false);
            let registration = Registration {
                id: id.clone(),
                registered: true,
            };
            Some(tokio::spawn(async move {
                super::lifecycle::observe(ctx, id, registration).await;
            }))
        }
    }
}

pub(super) struct Registration {
    id: String,
    registered: bool,
}
impl Registration {
    /// Consume a restart or deregister under the same lock used by ensure_observer.
    pub(super) fn restart_or_exit(&mut self) -> bool {
        let mut observers = registry().lock().unwrap();
        let restart = observers.get_mut(&self.id).expect("registered observer");
        if std::mem::take(restart) {
            true
        } else {
            observers.remove(&self.id);
            self.registered = false;
            false
        }
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        // Cancellation or panic must not strand the operation's registration.
        if self.registered {
            registry().lock().unwrap().remove(&self.id);
        }
    }
}

#[cfg(feature = "fixtures")]
mod counts {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    fn counts() -> &'static Mutex<HashMap<String, usize>> {
        static COUNTS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
        COUNTS.get_or_init(Default::default)
    }
    pub(crate) struct Live(pub(super) String);
    impl Live {
        pub(crate) fn new(id: &str) -> Self {
            *counts().lock().unwrap().entry(id.into()).or_default() += 1;
            Self(id.into())
        }
    }
    impl Drop for Live {
        fn drop(&mut self) {
            let mut counts = counts().lock().unwrap();
            let count = counts.get_mut(&self.0).unwrap();
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.0);
            }
        }
    }
    pub(crate) fn live_count(id: &str) -> usize {
        counts().lock().unwrap().get(id).copied().unwrap_or(0)
    }
}
#[cfg(feature = "fixtures")]
pub(super) use counts::{Live, live_count};
