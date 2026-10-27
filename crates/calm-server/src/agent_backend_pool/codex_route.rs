use super::codex::CodexBackends;
use provider::worker::{CodexDaemonProbe, CodexDaemonRoute};
use std::sync::Arc;

#[async_trait::async_trait]
impl CodexDaemonRoute for CodexBackends {
    async fn for_session(&self, id: &str) -> Option<Arc<dyn CodexDaemonProbe>> {
        CodexBackends::for_session(self, id)
            .await
            .inspect_err(|error| {
                tracing::warn!(%error, session_id = id, "Codex probe session route unresolved");
            })
            .ok()
            .map(|daemon| daemon.clone() as Arc<dyn CodexDaemonProbe>)
    }
    async fn for_thread(&self, id: &str) -> Option<Arc<dyn CodexDaemonProbe>> {
        CodexBackends::for_thread(self, id)
            .await
            .inspect_err(|error| {
                tracing::warn!(%error, thread_id = id, "Codex probe thread route unresolved");
            })
            .ok()
            .map(|daemon| daemon.clone() as Arc<dyn CodexDaemonProbe>)
    }
}
