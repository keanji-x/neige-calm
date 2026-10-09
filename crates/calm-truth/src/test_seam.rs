//! The one `fixtures`-only pause registry. Each layer names its own points (calm-truth's capture
//! persistence points in `capture_test_seam`, calm-server's in its `test_seams`) and arms them here.
//! The production binary compiles none of this.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Notify;

/// A one-shot pause a race test arms on a production path: the path calls [`pause_point`] with a
/// named point and the key it is working on, signals `entered`, and waits for `release`.
#[derive(Clone, Default)]
pub struct PausePoint {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}

type PauseRegistry = Mutex<HashMap<(String, String), PausePoint>>;

fn pause_points() -> &'static PauseRegistry {
    static POINTS: OnceLock<PauseRegistry> = OnceLock::new();
    POINTS.get_or_init(Default::default)
}

fn take(point: &str, key: &str) -> Option<PausePoint> {
    pause_points()
        .lock()
        .expect("pause point mutex")
        .remove(&(point.to_owned(), key.to_owned()))
}

/// Arm `hook` for the next request that reaches `point` working on `key`; the first one consumes it.
pub fn install_pause_for_test(point: &str, key: &str, hook: PausePoint) {
    pause_points()
        .lock()
        .expect("pause point mutex")
        .insert((point.to_owned(), key.to_owned()), hook);
}

/// Pause here when a test armed `point` for `key`. Call sites MUST be gated with
/// `#[cfg(feature = "fixtures")]` as a whole statement.
pub async fn pause_point(point: &str, key: &str) {
    if let Some(hook) = take(point, key) {
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}

/// The same observation-only pause for production paths already running on a blocking thread.
pub fn blocking_pause_point(point: &str, key: &str) {
    if let Some(hook) = take(point, key) {
        hook.entered.notify_one();
        tokio::runtime::Handle::current().block_on(hook.release.notified());
    }
}
