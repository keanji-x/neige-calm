//! Typed one-shot handshakes on the real capture path, keyed by card and record; fixtures only.
//! The points named here are calm-truth's persistence points; a higher layer names its own
//! lifecycle points by implementing [`CaptureSeamPoint`]. Both arm the one [`crate::test_seam`] registry.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::Notify;

use crate::test_seam::{PausePoint, install_pause_for_test, pause_point};

/// A named pause point on one capture, keyed by card and source record index.
pub trait CaptureSeamPoint: Copy {
    /// Registry name; unique across every layer's points.
    fn name(self) -> &'static str;
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum CapturePoint {
    ItemInserted,
    Committed,
    BeforeTransaction,
    Busy,
}

impl CaptureSeamPoint for CapturePoint {
    fn name(self) -> &'static str {
        match self {
            Self::ItemInserted => "capture-item-inserted",
            Self::Committed => "capture-committed",
            Self::BeforeTransaction => "capture-before-transaction",
            Self::Busy => "capture-busy",
        }
    }
}

fn key(card_id: &str, record_index: i64) -> String {
    format!("{card_id}#{record_index}")
}

pub fn install(card_id: &str, record_index: i64, point: impl CaptureSeamPoint) -> PausePoint {
    let pause = PausePoint::default();
    install_pause_for_test(point.name(), &key(card_id, record_index), pause.clone());
    pause
}

pub async fn reach(card_id: &str, record_index: i64, point: impl CaptureSeamPoint) {
    pause_point(point.name(), &key(card_id, record_index)).await;
}

/// Blocks the actual SQLite worker inside COMMIT, before durability/acknowledgement.
/// A bounded receive also releases the worker if a fixture assertion unwinds.
pub struct CommitPause {
    pub entered: Arc<Notify>,
    pub release: std::sync::mpsc::Sender<()>,
}
// Separate from `crate::test_seam`'s registry: the COMMIT hook runs on SQLite's worker thread and
// must block synchronously, so it holds a std mpsc receiver rather than an async pause.
type CommitKey = (String, i64);
type CommitHook = (Arc<Notify>, std::sync::mpsc::Receiver<()>);
fn commit_registry() -> &'static Mutex<HashMap<CommitKey, CommitHook>> {
    static HOOKS: OnceLock<Mutex<HashMap<CommitKey, CommitHook>>> = OnceLock::new();
    HOOKS.get_or_init(Default::default)
}
pub fn install_commit(card_id: &str, record_index: i64) -> CommitPause {
    let entered = Arc::new(Notify::new());
    let (release, receiver) = std::sync::mpsc::channel();
    commit_registry().lock().unwrap().insert(
        (card_id.to_owned(), record_index),
        (entered.clone(), receiver),
    );
    CommitPause { entered, release }
}
pub async fn arm_commit(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
    record_index: i64,
) -> Result<(), sqlx::Error> {
    let hook = commit_registry()
        .lock()
        .unwrap()
        .remove(&(card_id.to_owned(), record_index));
    if let Some((entered, receiver)) = hook {
        let mut receiver = Some(receiver);
        tx.lock_handle().await?.set_commit_hook(move || {
            if let Some(receiver) = receiver.take() {
                entered.notify_one();
                let _ = receiver.recv_timeout(std::time::Duration::from_secs(120));
            }
            true
        });
    }
    Ok(())
}
