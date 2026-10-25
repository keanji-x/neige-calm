//! Async, one-shot observations of a boot's provider pass, including cancellation.
use crate::provider_execution::{KnownEnd, ProviderExecution};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio::sync::oneshot;

#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    Returned,
    Dropped,
}
#[derive(Debug)]
pub enum Entered {
    Sweep,
    KnownEnd { settled: bool, claude_turn: bool },
}
struct Hook {
    entered: oneshot::Sender<Entered>,
    release: oneshot::Receiver<()>,
    exit: oneshot::Sender<Exit>,
}
pub struct Hold {
    pub entered: oneshot::Receiver<Entered>,
    release: oneshot::Sender<()>,
    pub exit: oneshot::Receiver<Exit>,
}
impl Hold {
    pub fn release(self) -> oneshot::Receiver<Exit> {
        // A stopped boot may already have dropped its receiver.
        let _ = self.release.send(());
        self.exit
    }
}
pub(crate) struct Pass(Option<oneshot::Sender<Exit>>);
impl Pass {
    pub(crate) fn returned(mut self) {
        if let Some(exit) = self.0.take() {
            let _ = exit.send(Exit::Returned);
        }
    }
}
impl Drop for Pass {
    fn drop(&mut self) {
        if let Some(exit) = self.0.take() {
            let _ = exit.send(Exit::Dropped);
        }
    }
}
#[derive(Default)]
struct Registry {
    next_boot: HashMap<String, (String, Hook)>,
    sweep: HashMap<usize, Hook>,
    driver: HashMap<(usize, String), Hook>,
}
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
fn hook() -> (Hook, Hold) {
    let (entered_tx, entered) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let (exit_tx, exit) = oneshot::channel();
    (
        Hook {
            entered: entered_tx,
            release: release_rx,
            exit: exit_tx,
        },
        Hold {
            entered,
            release,
            exit,
        },
    )
}
/// Consumed synchronously by exactly the next boot on this private database URL.
/// Binding happens before spawn, so the first tick cannot outrun installation.
pub fn hold_next_boot_sweep(db_url: &str, consumer: &str) -> Hold {
    let (hook, hold) = hook();
    assert!(
        registry()
            .lock()
            .unwrap()
            .next_boot
            .insert(db_url.into(), (consumer.into(), hook))
            .is_none()
    );
    hold
}
pub(crate) fn bind_boot(db_url: &str, owners: &[ProviderExecution]) {
    let mut registry = registry().lock().unwrap();
    if let Some((consumer, hook)) = registry.next_boot.remove(db_url) {
        let owner = owners
            .iter()
            .find(|owner| owner.owner.consumer() == consumer)
            .expect("registered provider owner");
        assert!(
            registry
                .sweep
                .insert(owner.drivers.fixture_key(), hook)
                .is_none()
        );
    }
}
pub fn hold_driver(owner: &ProviderExecution, session: &str) -> Hold {
    let (hook, hold) = hook();
    assert!(
        registry()
            .lock()
            .unwrap()
            .driver
            .insert((owner.drivers.fixture_key(), session.into()), hook)
            .is_none()
    );
    hold
}
async fn enter(hook: Option<Hook>, observed: Entered) -> Pass {
    let Some(hook) = hook else { return Pass(None) };
    // Construct before awaiting: abort while held is observable as Dropped.
    let pass = Pass(Some(hook.exit));
    let _ = hook.entered.send(observed);
    let _ = hook.release.await;
    pass
}
pub(crate) async fn sweep(owner: &ProviderExecution) -> Pass {
    let hook = registry()
        .lock()
        .unwrap()
        .sweep
        .remove(&owner.drivers.fixture_key());
    enter(hook, Entered::Sweep).await
}
pub(crate) async fn driver(
    owner: &ProviderExecution,
    session: &str,
    settled: bool,
    known: &Option<KnownEnd>,
) -> Pass {
    let hook = registry()
        .lock()
        .unwrap()
        .driver
        .remove(&(owner.drivers.fixture_key(), session.into()));
    enter(
        hook,
        Entered::KnownEnd {
            settled,
            claude_turn: matches!(known, Some(KnownEnd::ClaudeTurn(_))),
        },
    )
    .await
}
