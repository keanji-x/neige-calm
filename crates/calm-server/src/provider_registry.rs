use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use calm_exec::WorkerProvider;
use calm_provider::{ClaudeProvider, CodexDaemonProbe, CodexProvider, TerminalProvider};
use calm_types::worker::WorkerProviderKind;

use crate::shared_codex_appserver::SharedCodexAppServer;

#[derive(Clone)]
pub struct WorkerProviderRegistry {
    providers: Arc<HashMap<WorkerProviderKind, Arc<dyn WorkerProvider>>>,
}

impl WorkerProviderRegistry {
    pub fn new(
        supervisor_sock: impl Into<PathBuf>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
    ) -> Self {
        let supervisor_sock = supervisor_sock.into();
        let codex_daemon: Arc<dyn CodexDaemonProbe> = shared_codex_appserver;
        Self::from_entries([
            (
                WorkerProviderKind::Codex,
                Arc::new(CodexProvider::new(supervisor_sock.clone(), codex_daemon))
                    as Arc<dyn WorkerProvider>,
            ),
            (
                WorkerProviderKind::Claude,
                Arc::new(ClaudeProvider::new(supervisor_sock.clone())) as Arc<dyn WorkerProvider>,
            ),
            (
                WorkerProviderKind::Terminal,
                Arc::new(TerminalProvider::new(supervisor_sock)) as Arc<dyn WorkerProvider>,
            ),
        ])
    }

    pub fn from_entries<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (WorkerProviderKind, Arc<dyn WorkerProvider>)>,
    {
        Self {
            providers: Arc::new(entries.into_iter().collect()),
        }
    }

    pub fn get(&self, provider: WorkerProviderKind) -> Option<Arc<dyn WorkerProvider>> {
        self.providers.get(&provider).cloned()
    }
}
