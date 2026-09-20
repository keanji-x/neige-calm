//! Hold the next entry establishment between the supervisor's `Ready` and the renderer's attach, so
//! a test can put bytes into the attach replay before the reader exists.
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, oneshot};

static HOLD: Mutex<Option<(oneshot::Sender<String>, Arc<Notify>)>> = Mutex::new(None);

/// Arm one hold: `entered` carries the held terminal's id once the establishment reaches the hold;
/// `release` lets its attach proceed.
pub fn arm() -> (oneshot::Receiver<String>, Arc<Notify>) {
    let (entered, rx) = oneshot::channel();
    let release = Arc::new(Notify::new());
    *HOLD.lock().unwrap() = Some((entered, release.clone()));
    (rx, release)
}

pub(super) async fn hold(terminal_id: &str) {
    let armed = HOLD.lock().unwrap().take();
    if let Some((entered, release)) = armed {
        let _ = entered.send(terminal_id.to_owned());
        release.notified().await;
    }
}
