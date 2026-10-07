//! One-shot, worker-scoped pauses at the production receipt settlement boundary, and a tap on
//! the held-request messages of a worker's turns.
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
    pause_at(&SETTLEMENT, worker)
}
fn pause_at(hooks: &Mutex<HashMap<String, Hook>>, worker: &str) -> SettlementPause {
    let hook = Hook {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    hooks.lock().unwrap().insert(worker.into(), hook.clone());
    SettlementPause {
        entered: hook.entered,
        release: hook.release,
    }
}
pub(crate) async fn wait_at_settlement(worker: &str) {
    wait_at(&SETTLEMENT, worker).await;
}

static FENCED: LazyLock<Mutex<HashMap<String, Hook>>> = LazyLock::new(Mutex::default);

/// Pause `worker`'s next turn right after its approvals were fenced, before its requests are
/// reported gone and before any teardown: the agent process still runs.
pub fn pause_after_fence(worker: &str) -> SettlementPause {
    pause_at(&FENCED, worker)
}
pub(crate) async fn wait_after_fence(worker: &str) {
    wait_at(&FENCED, worker).await;
}

async fn wait_at(hooks: &Mutex<HashMap<String, Hook>>, worker: &str) {
    let hook = hooks.lock().unwrap().remove(worker);
    if let Some(hook) = hook {
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}

/// What a test sees of one held-request message an ACP turn pushed (#2348).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeldNote {
    Open {
        request_key: String,
        connection: String,
    },
    Gone {
        request_key: String,
    },
    ConnectionLost {
        connection: String,
    },
}
static HELD: LazyLock<Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<HeldNote>>>> =
    LazyLock::new(Mutex::default);

/// Observe, in order, the held-request messages of `worker`'s later turns. The messages still
/// reach the harness unchanged.
pub fn observe_held(worker: &str) -> tokio::sync::mpsc::UnboundedReceiver<HeldNote> {
    let (notes, observed) = tokio::sync::mpsc::unbounded_channel();
    HELD.lock().unwrap().insert(worker.into(), notes);
    observed
}
pub(crate) fn tap_held(
    worker: &str,
    held: crate::harness::held_requests::HeldRequestSender,
) -> crate::harness::held_requests::HeldRequestSender {
    use crate::harness::held_requests::HeldRequestMessage;
    let Some(notes) = HELD.lock().unwrap().get(worker).cloned() else {
        return held;
    };
    let (tapped, mut messages) = tokio::sync::mpsc::unbounded_channel::<HeldRequestMessage>();
    tokio::spawn(async move {
        while let Some(message) = messages.recv().await {
            let note = match &message {
                HeldRequestMessage::Open {
                    request_key,
                    connection,
                    ..
                } => HeldNote::Open {
                    request_key: request_key.0.clone(),
                    connection: connection.0.clone(),
                },
                HeldRequestMessage::Gone { request_key } => HeldNote::Gone {
                    request_key: request_key.0.clone(),
                },
                HeldRequestMessage::ConnectionLost { connection } => HeldNote::ConnectionLost {
                    connection: connection.0.clone(),
                },
            };
            let _ = notes.send(note);
            let _ = held.send(message);
        }
    });
    tapped
}
