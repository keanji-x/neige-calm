//! Advisory local source-loss subscriptions; identity and maintenance remain native facts.
use super::*;
use tokio::sync::Notify;

#[derive(Default)]
pub(super) struct SourceLoss {
    next: u64,
    consumers: HashMap<u64, Consumer>,
}
struct Consumer {
    signal: Weak<Notify>,
    provider: Option<provider::codex::LocalTerminationWatch>,
}

/// Dropping an old run removes only its own registration, including after replacement.
#[must_use]
pub(crate) struct SourceLossWatch {
    owner: Weak<NativeTurns>,
    token: u64,
}
impl Drop for SourceLossWatch {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.lock().source_loss.consumers.remove(&self.token);
        }
    }
}
impl NativeTurns {
    pub(in crate::shared_codex_appserver) fn watch_source_loss(
        self: &Arc<Self>,
        signal: &Arc<Notify>,
    ) -> SourceLossWatch {
        let mut state = self.lock();
        let client = state
            .source
            .as_ref()
            .filter(|source| state.is_current(source))
            .and_then(Weak::upgrade);
        let token = state.source_loss.next;
        state.source_loss.next = token.checked_add(1).expect("source-loss token exhausted");
        let provider = client.map(|client| client.watch_local_termination(signal));
        if provider.is_none() {
            signal.notify_one();
        }
        state.source_loss.consumers.insert(
            token,
            Consumer {
                signal: Arc::downgrade(signal),
                provider,
            },
        );
        SourceLossWatch {
            owner: Arc::downgrade(self),
            token,
        }
    }
}
impl SourceLoss {
    /// Called only after invalidate_now establishes the source-loss fact.
    pub(super) fn invalidate(&mut self) {
        for consumer in self.consumers.values_mut() {
            consumer.provider = None;
            if let Some(signal) = consumer.signal.upgrade() {
                signal.notify_one();
            }
        }
    }
    pub(super) fn bind(&mut self, client: &Arc<CodexAppServer>) {
        for consumer in self.consumers.values_mut() {
            consumer.provider = consumer
                .signal
                .upgrade()
                .map(|signal| client.watch_local_termination(&signal));
        }
    }
}
