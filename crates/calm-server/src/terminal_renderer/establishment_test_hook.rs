//! Pause actual fresh establishment after Spawned/PID/attach, before insertion.
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};
use tokio::sync::{Notify, oneshot};
struct Hook {
    entered: oneshot::Sender<String>,
    release: Arc<Notify>,
}
static HOOKS: LazyLock<Mutex<HashMap<String, Hook>>> = LazyLock::new(Mutex::default);
pub(crate) struct Override {
    task_id: String,
    release: Arc<Notify>,
}
impl Drop for Override {
    fn drop(&mut self) {
        HOOKS.lock().unwrap().remove(&self.task_id);
        self.release.notify_one();
    }
}
pub(crate) fn install(task_id: &str) -> (Override, oneshot::Receiver<String>, Arc<Notify>) {
    let (entered, rx) = oneshot::channel();
    let release = Arc::new(Notify::new());
    assert!(
        HOOKS
            .lock()
            .unwrap()
            .insert(
                task_id.into(),
                Hook {
                    entered,
                    release: release.clone()
                }
            )
            .is_none()
    );
    (
        Override {
            task_id: task_id.into(),
            release: release.clone(),
        },
        rx,
        release,
    )
}
pub(super) async fn pause(task_id: &str, terminal_id: &str) {
    let hook = HOOKS.lock().unwrap().remove(task_id);
    if let Some(hook) = hook {
        let _ = hook.entered.send(terminal_id.into());
        hook.release.notified().await;
    }
}
