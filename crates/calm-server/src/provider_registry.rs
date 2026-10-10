use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use calm_exec::WorkerProvider;
use calm_types::worker::WorkerProviderKind;
use provider::{ClaudeProvider, CodexDaemonProbe, CodexProvider, TerminalProvider};

use crate::shared_codex_appserver::SharedCodexAppServer;

#[derive(Clone)]
pub struct WorkerProviderRegistry {
    providers: Arc<HashMap<WorkerProviderKind, Arc<dyn WorkerProvider>>>,
}

#[async_trait::async_trait]
impl provider::worker::managed::ManagedSessionProbe for crate::harness::HarnessRegistry {
    async fn active_turn(&self, worker_session_id: &str) -> Option<Option<String>> {
        let handle = self.get(&worker_session_id.to_owned())?;
        let (snapshot, running) = handle.snapshot_with_running_turn().await;
        if snapshot.phase == calm_types::harness::HarnessPhaseTag::Wedged {
            return None;
        }
        Some(running.map(|turn| turn.turn_id))
    }
}

impl WorkerProviderRegistry {
    pub fn new(
        supervisor_sock: impl Into<PathBuf>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        harness: crate::harness::HarnessRegistry,
    ) -> Self {
        let supervisor_sock = supervisor_sock.into();
        let codex_daemon: Arc<dyn CodexDaemonProbe> = shared_codex_appserver;
        Self::from_entries([
            (
                WorkerProviderKind::OpenCode,
                Arc::new(provider::worker::managed::ManagedProvider::new(
                    "opencode",
                    Arc::new(harness),
                )) as Arc<dyn WorkerProvider>,
            ),
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

    /// Every registered provider's terminal-input declaration, ordered by provider name.
    pub fn tui_inputs(&self) -> Vec<(WorkerProviderKind, calm_exec::TuiInput)> {
        let mut declared: Vec<_> = self
            .providers
            .iter()
            .map(|(kind, provider)| (*kind, provider.tui_input()))
            .collect();
        declared.sort_by_key(|(kind, _)| kind.as_db_str());
        declared
    }

    /// The terminal-input declaration of `provider` (#2493); `None` for an unregistered kind.
    pub fn tui_input(&self, provider: WorkerProviderKind) -> Option<calm_exec::TuiInput> {
        self.providers
            .get(&provider)
            .map(|provider| provider.tui_input())
    }
}
