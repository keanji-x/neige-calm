//! Per-connection holds around the real pre-ready refusal path.
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};
use tokio::sync::{oneshot, watch};
use uuid::Uuid;

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub enum Stage {
    Enqueue,
    Delivery,
    Enqueued,
}
struct Hook {
    entered: Mutex<Option<oneshot::Sender<()>>>,
    release: watch::Receiver<bool>,
}
type Hooks = HashMap<(Uuid, Stage), Arc<Hook>>;
static HOOKS: LazyLock<Mutex<Hooks>> = LazyLock::new(Mutex::default);
pub struct Hold {
    client: Uuid,
    stage: Stage,
    release: watch::Sender<bool>,
}
impl Drop for Hold {
    fn drop(&mut self) {
        HOOKS.lock().unwrap().remove(&(self.client, self.stage));
        let _ = self.release.send(true);
    }
}
pub fn install(client: Uuid, stage: Stage) -> (Hold, oneshot::Receiver<()>) {
    let (entered, rx) = oneshot::channel();
    let (release, released) = watch::channel(false);
    assert!(
        HOOKS
            .lock()
            .unwrap()
            .insert(
                (client, stage),
                Arc::new(Hook {
                    entered: Mutex::new(Some(entered)),
                    release: released,
                })
            )
            .is_none()
    );
    (
        Hold {
            client,
            stage,
            release,
        },
        rx,
    )
}
pub(super) async fn pause(client: Option<Uuid>, stage: Stage) {
    let hook = client.and_then(|client| HOOKS.lock().unwrap().get(&(client, stage)).cloned());
    if let Some(hook) = hook {
        if let Some(entered) = hook.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        let mut released = hook.release.clone();
        let _ = released.wait_for(|released| *released).await;
    }
}
