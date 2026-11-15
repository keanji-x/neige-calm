use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use calm_exec::WorkerProvider;
use calm_types::runtime::AgentProtocol;
use calm_types::worker::WorkerProviderKind;
use provider::{ClaudeProvider, CodexProvider, TerminalProvider};

use crate::shared_codex_appserver::SharedCodexAppServer;

#[derive(Clone)]
pub struct WorkerProviderRegistry {
    providers: Arc<HashMap<WorkerProviderKind, Arc<dyn WorkerProvider>>>,
}

#[async_trait::async_trait]
impl provider::worker::managed::ManagedSessionProbe for crate::harness::HarnessRegistry {
    async fn active_turn(&self, worker_session_id: &str) -> Option<Option<String>> {
        let handle = self.get(worker_session_id)?;
        let (snapshot, running) = handle.snapshot_with_running_turn().await.ok()?;
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
        Self::with_route(
            supervisor_sock,
            Arc::new(provider::worker::FixedCodexDaemon(shared_codex_appserver)),
            harness,
        )
    }
    pub fn with_route(
        supervisor_sock: impl Into<PathBuf>,
        route: Arc<dyn provider::worker::CodexDaemonRoute>,
        harness: crate::harness::HarnessRegistry,
    ) -> Self {
        let supervisor_sock = supervisor_sock.into();
        let harness = Arc::new(harness);
        // Every ACP provider's conversations live in the managed Harness.
        let acp = AgentProtocol::Acp.providers().map(|provider| {
            (
                WorkerProviderKind::from(&provider),
                Arc::new(provider::worker::managed::ManagedProvider::new(
                    provider.wire_name(),
                    harness.clone(),
                )) as Arc<dyn WorkerProvider>,
            )
        });
        Self::from_entries(acp.chain([
            (
                WorkerProviderKind::Codex,
                Arc::new(CodexProvider::with_route(supervisor_sock.clone(), route))
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
        ]))
    }

    pub fn for_backends(
        daemon: &crate::state::DaemonClient,
        backends: Arc<crate::agent_backend_pool::codex::CodexBackends>,
        harness: crate::harness::HarnessRegistry,
    ) -> Self {
        Self::with_route(Self::supervisor_sock(daemon), backends, harness)
    }

    /// The registry for `daemon`'s proc supervisor: the one constructor the Dispatcher and the
    /// terminal tools share.
    pub fn for_daemon(
        daemon: &crate::state::DaemonClient,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        harness: crate::harness::HarnessRegistry,
    ) -> Self {
        Self::new(
            Self::supervisor_sock(daemon),
            shared_codex_appserver,
            harness,
        )
    }

    fn supervisor_sock(daemon: &crate::state::DaemonClient) -> PathBuf {
        daemon.proc_supervisor_sock.clone().unwrap_or_else(|| {
            std::env::temp_dir().join("neige-reaper-missing-proc-supervisor.sock")
        })
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

struct NativeWorkerProbe {
    host: Arc<crate::acp_session::host::AcpHost>,
    harness: crate::harness::HarnessRegistry,
}
#[async_trait::async_trait]
impl provider::worker::managed::ManagedSessionProbe for NativeWorkerProbe {
    async fn active_turn(&self, session: &str) -> Option<Option<String>> {
        if let Some(turn) = self.host.native_workers.active_turn(session) {
            return Some(turn);
        }
        provider::worker::managed::ManagedSessionProbe::active_turn(&self.harness, session).await
    }
}
impl WorkerProviderRegistry {
    pub fn with_native_workers(
        mut self,
        host: Arc<crate::acp_session::host::AcpHost>,
        harness: crate::harness::HarnessRegistry,
    ) -> Self {
        let probe = Arc::new(NativeWorkerProbe { host, harness });
        let entries = Arc::make_mut(&mut self.providers);
        for cap in calm_types::provider_worker::PROVIDER_WORKER_CAPABILITIES
            .iter()
            .filter(|cap| {
                cap.message_transport
                    == calm_types::provider_worker::WorkerMessageTransport::NativeSession
            })
        {
            entries.insert(
                WorkerProviderKind::from(&cap.provider),
                Arc::new(provider::worker::managed::ManagedProvider::new(
                    cap.provider.wire_name(),
                    probe.clone(),
                )),
            );
        }
        self
    }
}
