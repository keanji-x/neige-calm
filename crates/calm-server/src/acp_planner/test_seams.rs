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
