//! Typed one-shot handshakes on the real capture persistence path; fixtures only.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum CapturePoint {
    ItemInserted,
    Committed,
    BeforeTransaction,
    Busy,
    Idle,
}

#[derive(Clone, Default)]
pub struct CapturePause {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}

type Key = (String, i64, CapturePoint);
fn registry() -> &'static Mutex<HashMap<Key, CapturePause>> {
    static POINTS: OnceLock<Mutex<HashMap<Key, CapturePause>>> = OnceLock::new();
    POINTS.get_or_init(Default::default)
}

pub fn install(card_id: &str, record_index: i64, point: CapturePoint) -> CapturePause {
    let pause = CapturePause::default();
    registry()
        .lock()
        .unwrap()
        .insert((card_id.to_owned(), record_index, point), pause.clone());
    pause
}

pub async fn reach(card_id: &str, record_index: i64, point: CapturePoint) {
    let pause = registry()
        .lock()
        .unwrap()
        .remove(&(card_id.to_owned(), record_index, point));
    if let Some(pause) = pause {
        pause.entered.notify_one();
        pause.release.notified().await;
    }
}
