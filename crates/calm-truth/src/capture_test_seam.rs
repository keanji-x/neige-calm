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
    CheckpointLoaded,
    ReplacementReady,
    CancellationSettling,
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

/// Blocks the actual SQLite worker inside COMMIT, before durability/acknowledgement.
/// A bounded receive also releases the worker if a fixture assertion unwinds.
pub struct CommitPause {
    pub entered: Arc<Notify>,
    pub release: std::sync::mpsc::Sender<()>,
}
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
