//! One-shot, worker-scoped pauses at the production receipt settlement boundary.
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::Notify;

#[derive(Clone)]
struct Hook {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
static SETTLEMENT: LazyLock<Mutex<HashMap<String, Hook>>> = LazyLock::new(Mutex::default);

pub struct SettlementPause {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}
impl Drop for SettlementPause {
    fn drop(&mut self) {
        self.release.notify_one();
    }
}
pub fn pause_settlement(worker: &str) -> SettlementPause {
    let hook = Hook {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    SETTLEMENT
        .lock()
        .unwrap()
        .insert(worker.into(), hook.clone());
    SettlementPause {
        entered: hook.entered,
        release: hook.release,
    }
}
pub(crate) async fn wait_at_settlement(worker: &str) {
    let hook = SETTLEMENT.lock().unwrap().remove(worker);
    if let Some(hook) = hook {
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}
