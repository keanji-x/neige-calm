//! Detector-only delivery through the boot-bound canonical System owner.
use crate::conversation_keys::derive_track_conversation_keys;
use crate::error::{CalmError, Result};
use crate::state::{AppState, RouteState};
use crate::worker_quiet::{ObservedQuietEpisode, QuietWorkerInbox};
use futures::future::BoxFuture;
use std::sync::Arc;

pub const WORKER_WATCHER_CONVERSATION_KEY: &str = "worker-watcher";
pub(crate) const BOOTSTRAP_TEXT: &str =
    include_str!("../../prompts/terminal/worker-watcher-bootstrap.md");

pub fn watcher_card_id(track_id: &str) -> String {
    derive_track_conversation_keys(track_id, WORKER_WATCHER_CONVERSATION_KEY).card_id
}

pub struct WorkerWatcher {
    route: RouteState,
    renderer: Arc<crate::terminal_renderer::TerminalRendererRegistry>,
    issuer: Option<crate::dispatcher::system_assistant::SystemAssistantIssuer>,
}
impl WorkerWatcher {
    pub fn new(state: &AppState) -> Self {
        use axum::extract::FromRef;
        Self {
            route: RouteState::from_ref(state),
            renderer: state.terminal_renderer.clone(),
            issuer: state.worker_watcher_issuer.clone(),
        }
    }
    pub async fn deliver(&self, episode: ObservedQuietEpisode) -> Result<()> {
        self.issuer
            .as_ref()
            .ok_or_else(|| CalmError::Forbidden("Watcher production boot is incomplete".into()))?
            .submit_quiet(&self.route, self.renderer.clone(), episode)
            .await
            .map(|_| ())
    }
}
impl QuietWorkerInbox for WorkerWatcher {
    fn deliver<'a>(&'a self, episode: ObservedQuietEpisode) -> BoxFuture<'a, Result<()>> {
        Box::pin(WorkerWatcher::deliver(self, episode))
    }
}
