pub mod claude_normalizer;
pub mod claude_transcript;
pub mod codex_normalizer;
pub mod codex_rollout;
pub mod cursor;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calm_exec::flow::{WorkerFlowItemSink, WorkerFlowSource};
use calm_truth::worker_flow_sink::WorkerFlowSink;
use calm_types::error::CoreError;
use calm_types::event::Event;
use calm_types::runtime::{AgentProvider, CardRuntime, RunStatus, RuntimeKind};
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSession, WorkerSessionId,
    WorkerSessionState,
};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::db::Repo;
use crate::event::EventBus;
use crate::model::Card;
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::state::AppState;

use self::claude_transcript::{ClaudeTranscriptFlowSource, ClaudeTranscriptFlowSourceOptions};
use self::codex_rollout::{CodexRolloutFlowSource, CodexRolloutFlowSourceOptions};

pub struct WorkerFlowDriver {
    repo: Arc<dyn Repo>,
    shared_codex_appserver: Arc<SharedCodexAppServer>,
    sink: Arc<dyn WorkerFlowItemSink>,
    events: EventBus,
    tasks: Mutex<HashMap<String, SourceTask>>,
    subscriber_started: AtomicBool,
    flow_options: CodexRolloutFlowSourceOptions,
    claude_flow_options: ClaudeTranscriptFlowSourceOptions,
}

struct SourceTask {
    stop: CancellationToken,
    join: JoinHandle<()>,
}

impl WorkerFlowDriver {
    pub fn new(
        repo: Arc<dyn Repo>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        sink: Arc<dyn WorkerFlowItemSink>,
        events: EventBus,
    ) -> Arc<Self> {
        Arc::new(Self {
            repo,
            shared_codex_appserver,
            sink,
            events,
            tasks: Mutex::new(HashMap::new()),
            subscriber_started: AtomicBool::new(false),
            flow_options: CodexRolloutFlowSourceOptions::default(),
            claude_flow_options: ClaudeTranscriptFlowSourceOptions::default(),
        })
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn new_with_flow_options_for_test(
        repo: Arc<dyn Repo>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        sink: Arc<dyn WorkerFlowItemSink>,
        events: EventBus,
        flow_options: CodexRolloutFlowSourceOptions,
    ) -> Arc<Self> {
        Arc::new(Self {
            repo,
            shared_codex_appserver,
            sink,
            events,
            tasks: Mutex::new(HashMap::new()),
            subscriber_started: AtomicBool::new(false),
            flow_options,
            claude_flow_options: ClaudeTranscriptFlowSourceOptions::default(),
        })
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn new_with_source_options_for_test(
        repo: Arc<dyn Repo>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        sink: Arc<dyn WorkerFlowItemSink>,
        events: EventBus,
        flow_options: CodexRolloutFlowSourceOptions,
        claude_flow_options: ClaudeTranscriptFlowSourceOptions,
    ) -> Arc<Self> {
        Arc::new(Self {
            repo,
            shared_codex_appserver,
            sink,
            events,
            tasks: Mutex::new(HashMap::new()),
            subscriber_started: AtomicBool::new(false),
            flow_options,
            claude_flow_options,
        })
    }

    pub fn from_state_parts(
        repo: Arc<dyn Repo>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        events: EventBus,
    ) -> Arc<Self> {
        let sink: Arc<dyn WorkerFlowItemSink> = Arc::new(WorkerFlowSink::new(repo.clone()));
        Self::new(repo, shared_codex_appserver, sink, events)
    }

    pub async fn start_on_boot(self: &Arc<Self>) -> Result<(), CoreError> {
        let mut runtimes = self
            .repo
            .runtimes_active_for_kind(RuntimeKind::CodexCard)
            .await
            .map_err(|e| CoreError::Internal(format!("runtimes_active_for_kind codex: {e}")))?;
        runtimes.extend(
            self.repo
                .runtimes_active_for_kind(RuntimeKind::ClaudeCard)
                .await
                .map_err(|e| {
                    CoreError::Internal(format!("runtimes_active_for_kind claude: {e}"))
                })?,
        );
        for runtime in runtimes {
            if let Err(err) = self.attach_runtime(runtime).await {
                tracing::warn!(error = %err, "worker-flow boot attach failed");
            }
        }
        self.start_runtime_subscriber();
        Ok(())
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub async fn tasks_alive_for_test(&self) -> usize {
        let tasks = self.tasks.lock().await;
        tasks
            .values()
            .filter(|task| !task.stop.is_cancelled() && !task.join.is_finished())
            .count()
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub async fn task_stop_tokens_for_test(&self) -> Vec<CancellationToken> {
        let tasks = self.tasks.lock().await;
        tasks.values().map(|task| task.stop.clone()).collect()
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub async fn attach_runtime_for_test(&self, runtime: CardRuntime) -> Result<(), CoreError> {
        self.attach_runtime(runtime).await
    }

    fn start_runtime_subscriber(self: &Arc<Self>) {
        if self.subscriber_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut rx = match weak.upgrade() {
                Some(driver) => driver.events.subscribe_filtered(),
                None => return,
            };
            loop {
                match rx.recv().await {
                    Ok(env) => {
                        let Some(driver) = weak.upgrade() else {
                            return;
                        };
                        driver.handle_event(env.event).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            skipped,
                            "worker-flow runtime subscriber lagged; future runtime events remain idempotent"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    async fn handle_event(&self, event: Event) {
        match event {
            Event::RuntimeStarted {
                runtime_id,
                kind,
                agent_provider,
                ..
            } if is_supported_runtime_pair(&kind, agent_provider.as_ref()) => {
                match self.repo.runtime_get_by_id(&runtime_id).await {
                    Ok(Some(runtime)) => {
                        if let Err(err) = self.attach_runtime(runtime).await {
                            tracing::warn!(
                                runtime_id,
                                error = %err,
                                "worker-flow runtime-start attach failed"
                            );
                        }
                    }
                    Ok(None) => tracing::warn!(
                        runtime_id,
                        "worker-flow runtime-start event had no runtime row"
                    ),
                    Err(err) => tracing::warn!(
                        runtime_id,
                        error = %err,
                        "worker-flow runtime lookup failed"
                    ),
                }
            }
            Event::RuntimeStatusChanged {
                card_id,
                new_status: RunStatus::Exited | RunStatus::Failed | RunStatus::Superseded,
                ..
            } => {
                self.cancel_card(&card_id).await;
            }
            Event::RuntimeStatusChanged {
                runtime_id,
                new_status: RunStatus::Running | RunStatus::Idle | RunStatus::TurnPending,
                ..
            } => match self.repo.runtime_get_by_id(&runtime_id).await {
                Ok(Some(runtime)) if is_supported_runtime(&runtime) => {
                    if let Err(err) = self.attach_runtime(runtime).await {
                        tracing::warn!(
                            runtime_id,
                            error = %err,
                            "worker-flow runtime-status attach failed"
                        );
                    }
                }
                Ok(_) => {}
                Err(err) => tracing::warn!(
                    runtime_id,
                    error = %err,
                    "worker-flow runtime-status lookup failed"
                ),
            },
            Event::CardAdded(card) if card.kind == "codex" || card.kind == "claude" => {
                let card_id = card.id.to_string();
                match self.repo.runtime_get_active_for_card(&card_id).await {
                    Ok(Some(runtime)) if is_supported_runtime(&runtime) => {
                        if let Err(err) = self.attach_runtime(runtime).await {
                            tracing::warn!(
                                card_id = %card_id,
                                error = %err,
                                "worker-flow card-added attach failed"
                            );
                        }
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => tracing::warn!(
                        card_id = %card_id,
                        "worker-flow CardAdded event had no active runtime row"
                    ),
                    Err(err) => tracing::warn!(
                        card_id = %card_id,
                        error = %err,
                        "worker-flow card-added runtime lookup failed"
                    ),
                }
            }
            Event::RuntimeSuperseded {
                new_runtime_id,
                card_id,
                ..
            } => {
                self.cancel_card(&card_id).await;
                match self.repo.runtime_get_by_id(&new_runtime_id).await {
                    Ok(Some(runtime)) if is_supported_runtime(&runtime) => {
                        if let Err(err) = self.attach_runtime(runtime).await {
                            tracing::warn!(
                                runtime_id = %new_runtime_id,
                                error = %err,
                                "worker-flow superseded attach failed"
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(err) => tracing::warn!(
                        runtime_id = %new_runtime_id,
                        error = %err,
                        "worker-flow superseded runtime lookup failed"
                    ),
                }
            }
            _ => {}
        }
    }

    async fn attach_runtime(&self, runtime: CardRuntime) -> Result<(), CoreError> {
        let Some(source_kind) = source_kind_for_runtime(&runtime) else {
            return Ok(());
        };
        match source_kind {
            FlowSourceKind::Codex if runtime.thread_id.is_none() => {
                tracing::warn!(
                    card_id = %runtime.card_id,
                    runtime_id = %runtime.id,
                    "worker-flow codex runtime has no thread_id; skipping rollout attach"
                );
                return Ok(());
            }
            FlowSourceKind::Claude if runtime.session_id.is_none() => {
                tracing::warn!(
                    card_id = %runtime.card_id,
                    runtime_id = %runtime.id,
                    "worker-flow claude runtime has no session_id; skipping transcript attach"
                );
                return Ok(());
            }
            _ => {}
        }

        {
            let mut tasks = self.tasks.lock().await;
            tasks.retain(|_, task| !task.join.is_finished() && !task.stop.is_cancelled());
            if tasks.contains_key(&runtime.card_id) {
                return Ok(());
            }
        }

        let card = self
            .repo
            .card_get(&runtime.card_id)
            .await
            .map_err(|e| CoreError::Internal(format!("card_get: {e}")))?
            .ok_or_else(|| CoreError::NotFound(format!("card {}", runtime.card_id)))?;
        let session = session_from_runtime(&runtime, &card);
        let stop = CancellationToken::new();
        let sink = self.sink.clone();
        let card_id = runtime.card_id.clone();
        let join = match source_kind {
            FlowSourceKind::Codex => {
                let source = CodexRolloutFlowSource::new_with_options(
                    self.repo.clone(),
                    runtime.clone(),
                    self.shared_codex_appserver.codex_home_path().to_path_buf(),
                    stop.clone(),
                    self.flow_options.clone(),
                );
                tokio::spawn(async move {
                    if let Err(err) = source.capture(&session, sink.as_ref()).await {
                        tracing::warn!(
                            card_id = %card_id,
                            error = %err,
                            "worker-flow codex rollout source stopped with error"
                        );
                    }
                })
            }
            FlowSourceKind::Claude => {
                let card_cwd = self.resolve_card_cwd(&card).await;
                let source = ClaudeTranscriptFlowSource::new_with_options(
                    self.repo.clone(),
                    runtime.clone(),
                    card_cwd,
                    stop.clone(),
                    self.claude_flow_options.clone(),
                );
                tokio::spawn(async move {
                    if let Err(err) = source.capture(&session, sink.as_ref()).await {
                        tracing::warn!(
                            card_id = %card_id,
                            error = %err,
                            "worker-flow claude transcript source stopped with error"
                        );
                    }
                })
            }
        };

        let mut tasks = self.tasks.lock().await;
        tasks.insert(runtime.card_id.clone(), SourceTask { stop, join });
        Ok(())
    }

    async fn resolve_card_cwd(&self, card: &Card) -> String {
        if let Some(cwd) = card_payload_string(card, "cwd") {
            return cwd;
        }

        if let Some(terminal_id) = card_payload_string(card, "terminal_id") {
            match self.repo.terminal_get(&terminal_id).await {
                Ok(Some(term)) if !term.cwd.is_empty() => return term.cwd,
                Ok(_) => {}
                Err(err) => tracing::warn!(
                    card_id = %card.id,
                    terminal_id = %terminal_id,
                    error = %err,
                    "worker-flow failed to read terminal cwd for card"
                ),
            }
        }

        crate::routes::codex_cards::default_cwd()
    }

    async fn cancel_card(&self, card_id: &str) {
        let task = self.tasks.lock().await.remove(card_id);
        if let Some(task) = task {
            task.stop.cancel();
        }
    }
}

impl Drop for WorkerFlowDriver {
    fn drop(&mut self) {
        if let Ok(tasks) = self.tasks.try_lock() {
            for task in tasks.values() {
                task.stop.cancel();
            }
        }
    }
}

#[derive(Clone, Copy)]
enum FlowSourceKind {
    Codex,
    Claude,
}

fn source_kind_for_runtime(runtime: &CardRuntime) -> Option<FlowSourceKind> {
    match (&runtime.kind, runtime.agent_provider.as_ref()) {
        (RuntimeKind::CodexCard, Some(AgentProvider::Codex)) => Some(FlowSourceKind::Codex),
        (RuntimeKind::ClaudeCard, Some(AgentProvider::Claude)) => Some(FlowSourceKind::Claude),
        _ => None,
    }
}

fn is_supported_runtime(runtime: &CardRuntime) -> bool {
    source_kind_for_runtime(runtime).is_some()
}

fn is_supported_runtime_pair(kind: &RuntimeKind, provider: Option<&AgentProvider>) -> bool {
    matches!(
        (kind, provider),
        (RuntimeKind::CodexCard, Some(AgentProvider::Codex))
            | (RuntimeKind::ClaudeCard, Some(AgentProvider::Claude))
    )
}

fn card_payload_string(card: &Card, key: &str) -> Option<String> {
    card.payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub async fn start_on_boot(state: &AppState) -> Result<(), CoreError> {
    state.worker_flow.start_on_boot().await
}

fn session_from_runtime(runtime: &CardRuntime, card: &Card) -> WorkerSession {
    let id = runtime
        .session_id
        .clone()
        .or_else(|| runtime.thread_id.clone())
        .unwrap_or_else(|| runtime.id.clone());
    WorkerSession {
        id: WorkerSessionId::from(id),
        wave_id: card.wave_id.clone(),
        provider: worker_provider_from_runtime(runtime),
        mode: SessionMode::Resumable,
        contract: WorkerContract::Executor,
        parent_session_id: None,
        requester_session_id: None,
        state: worker_state_from_runtime(runtime.status.clone()),
        mcp_token_hash: None,
        thread_id: runtime.thread_id.clone(),
        agent_session_id: runtime.session_id.clone(),
        active_turn_id: runtime.active_turn_id.clone(),
        terminal_run_id: runtime.terminal_run_id.clone(),
        handle_state_json: runtime.handle_state_json.clone(),
        liveness: liveness_from_runtime(runtime.status.clone()),
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        created_at_ms: runtime.created_at_ms,
        updated_at_ms: runtime.updated_at_ms,
        completed_at_ms: runtime.completed_at_ms,
    }
}

fn worker_provider_from_runtime(runtime: &CardRuntime) -> WorkerProviderKind {
    match runtime.agent_provider.as_ref() {
        Some(AgentProvider::Claude) => WorkerProviderKind::Claude,
        Some(AgentProvider::Codex) | None => WorkerProviderKind::Codex,
    }
}

fn worker_state_from_runtime(status: RunStatus) -> WorkerSessionState {
    match status {
        RunStatus::Starting => WorkerSessionState::Starting,
        RunStatus::Running => WorkerSessionState::Running,
        RunStatus::Idle => WorkerSessionState::Idle,
        RunStatus::TurnPending => WorkerSessionState::TurnPending,
        RunStatus::Failed => WorkerSessionState::Failed,
        RunStatus::Exited => WorkerSessionState::Exited,
        RunStatus::Superseded => WorkerSessionState::Superseded,
    }
}

fn liveness_from_runtime(status: RunStatus) -> LivenessTag {
    match status {
        RunStatus::Starting | RunStatus::Running | RunStatus::TurnPending => LivenessTag::Alive,
        RunStatus::Idle => LivenessTag::Idle,
        RunStatus::Failed | RunStatus::Exited | RunStatus::Superseded => LivenessTag::Exited,
    }
}
