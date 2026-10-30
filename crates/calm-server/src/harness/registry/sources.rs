//! Exact accepted-input sources and producer-owned handoff lifetimes belong to
//! the original worker, even while its native slot is dormant. SQL owns order.
use crate::conversation_inputs::InputSource;
use calm_types::conversation::CanonicalPendingIdentity;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// Availability is not admission: every Ready source is revalidated at the
/// protected read, claim and native crossing. A handoff grants nothing.
pub(crate) enum SourceReadiness {
    Ready(InputSource),
    AwaitingHandoff,
    NeedsSource,
}

#[derive(Default)]
struct State {
    sources: HashMap<CanonicalPendingIdentity, InputSource>,
    handoffs: usize,
}
#[derive(Default)]
pub(crate) struct CanonicalSourceIndex {
    state: Mutex<State>,
    changed: Notify,
}

/// The producer acquires this before its transaction commits, then transfers it
/// synchronously to the post-commit owner. Rollback/cancellation also releases
/// it. It represents the worker's existing batch source discovery, not a queue
/// entry or permission for any input. It is deliberately not Clone.
pub(crate) struct SourceHandoff(Arc<CanonicalSourceIndex>);
impl Drop for SourceHandoff {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("canonical source index");
        state.handoffs = state.handoffs.checked_sub(1).expect("owned source handoff");
        drop(state);
        self.0.changed.notify_one();
    }
}
impl CanonicalSourceIndex {
    pub(crate) fn begin_handoff(self: &Arc<Self>) -> SourceHandoff {
        self.state.lock().expect("canonical source index").handoffs += 1;
        self.changed.notify_one();
        SourceHandoff(self.clone())
    }
    pub(crate) async fn changed(&self) {
        self.changed.notified().await;
    }
    pub(crate) fn retire(&self) {
        // Outstanding guards own their counts until Drop, including after
        // retirement. Never reset a counter that a late owner must release.
        self.state
            .lock()
            .expect("canonical source index")
            .sources
            .clear();
        self.changed.notify_one();
    }
    pub(crate) fn is_empty(&self) -> bool {
        let state = self.state.lock().expect("canonical source index");
        state.sources.is_empty() && state.handoffs == 0
    }
    pub(crate) fn observe(&self, input: CanonicalPendingIdentity, source: InputSource) {
        let source = source.for_identity(&input);
        self.state
            .lock()
            .expect("canonical source index")
            .sources
            .insert(input, source);
        self.changed.notify_one();
    }
    pub(crate) fn readiness(&self, input: &CanonicalPendingIdentity) -> SourceReadiness {
        let state = self.state.lock().expect("canonical source index");
        // An exact source always wins: another in-flight producer cannot hide
        // revocation of a bound credential or qualify someone else's input.
        match state.sources.get(input) {
            Some(source) => SourceReadiness::Ready(source.clone()),
            None if state.handoffs > 0 => SourceReadiness::AwaitingHandoff,
            None => SourceReadiness::NeedsSource,
        }
    }
    #[cfg(test)]
    pub(crate) fn source(&self, input: &CanonicalPendingIdentity) -> Option<InputSource> {
        match self.readiness(input) {
            SourceReadiness::Ready(source) => Some(source),
            SourceReadiness::AwaitingHandoff | SourceReadiness::NeedsSource => None,
        }
    }
    /// Only exact references returned from committed consumption are removed.
    /// An older runtime projection must never prune a concurrently accepted tail.
    pub(crate) fn consumed(&self, inputs: &[CanonicalPendingIdentity]) {
        let mut state = self.state.lock().expect("canonical source index");
        for input in inputs {
            state.sources.remove(input);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> CanonicalPendingIdentity {
        calm_types::conversation::MessageRevisionRef {
            message_id: calm_types::conversation::ConversationMessageId::parse(&"a".repeat(32))
                .unwrap(),
            revision: std::num::NonZeroU32::new(1).unwrap(),
        }
        .into()
    }

    #[tokio::test]
    async fn handoff_lifetime_distinguishes_pending_from_missing_and_wakes_on_last_drop() {
        let index = Arc::new(CanonicalSourceIndex::default());
        let input = input();
        assert!(matches!(
            index.readiness(&input),
            SourceReadiness::NeedsSource
        ));
        let first = index.begin_handoff();
        let second = index.begin_handoff();
        assert!(
            !index.is_empty(),
            "a reservation cannot retire an owned handoff"
        );
        assert!(matches!(
            index.readiness(&input),
            SourceReadiness::AwaitingHandoff
        ));
        drop(first);
        assert!(matches!(
            index.readiness(&input),
            SourceReadiness::AwaitingHandoff
        ));
        // Drain the coalesced earlier notification, then observe final release.
        index.changed().await;
        drop(second);
        tokio::time::timeout(std::time::Duration::from_secs(1), index.changed())
            .await
            .unwrap();
        assert!(matches!(
            index.readiness(&input),
            SourceReadiness::NeedsSource
        ));
        assert!(index.is_empty());
    }

    #[tokio::test]
    async fn retirement_and_cancelled_producer_release_each_owned_guard_once() {
        let index = Arc::new(CanonicalSourceIndex::default());
        let guard = index.begin_handoff();
        let (started, observed) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(async move {
            let _guard = guard;
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        observed.await.unwrap();
        index.retire();
        assert!(!index.is_empty());
        owner.abort();
        assert!(owner.await.unwrap_err().is_cancelled());
        assert!(index.is_empty());
        assert!(matches!(
            index.readiness(&input()),
            SourceReadiness::NeedsSource
        ));
    }

    #[tokio::test]
    async fn bound_source_wins_over_handoffs_and_retains_real_session_revocation() {
        use crate::auth::{AuthConfig, AuthState, SessionAuthority, SessionIdentity};
        let auth = AuthState::new(AuthConfig {
            username: Some("owner".into()),
            password: Some("fixture".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        });
        let token = auth.sessions.create(
            SessionIdentity::owner(&auth.config),
            SessionAuthority::PasswordLogin,
        );
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("{}={token}", crate::auth::SESSION_COOKIE)
                .parse()
                .unwrap(),
        );
        let (_, source) = crate::auth::authenticated_source(auth.clone(), headers).await;
        let index = Arc::new(CanonicalSourceIndex::default());
        let input = input();
        let guard = index.begin_handoff();
        index.observe(input.clone(), source.unwrap().into());
        let SourceReadiness::Ready(source) = index.readiness(&input) else {
            panic!("exact source must win");
        };
        source.check_current().unwrap();
        auth.sessions.remove(&token).unwrap();
        let SourceReadiness::Ready(source) = index.readiness(&input) else {
            panic!("handoff cannot mask a revoked exact source");
        };
        assert!(matches!(
            source.check_current(),
            Err(crate::error::CalmError::Unauthorized)
        ));
        drop(guard);
        index.consumed(std::slice::from_ref(&input));
        assert!(matches!(
            index.readiness(&input),
            SourceReadiness::NeedsSource
        ));
    }
}
