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

use self::codex_rollout::CodexRolloutFlowSource;

pub struct WorkerFlowDriver {
    repo: Arc<dyn Repo>,
    shared_codex_appserver: Arc<SharedCodexAppServer>,
    sink: Arc<dyn WorkerFlowItemSink>,
    events: EventBus,
    tasks: Mutex<HashMap<String, SourceTask>>,
    subscriber_started: AtomicBool,
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
        let runtimes = self
            .repo
            .runtimes_active_for_kind(RuntimeKind::CodexCard)
            .await
            .map_err(|e| CoreError::Internal(format!("runtimes_active_for_kind: {e}")))?;
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

    fn start_runtime_subscriber(self: &Arc<Self>) {
        if self.subscriber_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let driver = Arc::clone(self);
        tokio::spawn(async move {
            driver.run_runtime_subscriber().await;
        });
    }

    async fn run_runtime_subscriber(self: Arc<Self>) {
        let mut rx = self.events.subscribe_filtered();
        loop {
            match rx.recv().await {
                Ok(env) => self.handle_event(env.event).await,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        skipped,
                        "worker-flow runtime subscriber lagged; future runtime events remain idempotent"
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    async fn handle_event(&self, event: Event) {
        match event {
            Event::RuntimeStarted {
                runtime_id,
                kind,
                agent_provider,
                ..
            } if kind == RuntimeKind::CodexCard && agent_provider == Some(AgentProvider::Codex) => {
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
            Event::RuntimeSuperseded {
                new_runtime_id,
                card_id,
                ..
            } => {
                self.cancel_card(&card_id).await;
                match self.repo.runtime_get_by_id(&new_runtime_id).await {
                    Ok(Some(runtime))
                        if runtime.kind == RuntimeKind::CodexCard
                            && runtime.agent_provider == Some(AgentProvider::Codex) =>
                    {
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
        if runtime.agent_provider != Some(AgentProvider::Codex)
            || runtime.kind != RuntimeKind::CodexCard
        {
            return Ok(());
        }
        if runtime.thread_id.is_none() {
            tracing::warn!(
                card_id = %runtime.card_id,
                runtime_id = %runtime.id,
                "worker-flow codex runtime has no thread_id; skipping rollout attach"
            );
            return Ok(());
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
        let source = CodexRolloutFlowSource::new(
            self.repo.clone(),
            runtime.clone(),
            self.shared_codex_appserver.codex_home_path().to_path_buf(),
            stop.clone(),
        );
        let sink = self.sink.clone();
        let card_id = runtime.card_id.clone();
        let join = tokio::spawn(async move {
            if let Err(err) = source.capture(&session, sink.as_ref()).await {
                tracing::warn!(
                    card_id = %card_id,
                    error = %err,
                    "worker-flow codex rollout source stopped with error"
                );
            }
        });

        let mut tasks = self.tasks.lock().await;
        tasks.insert(runtime.card_id.clone(), SourceTask { stop, join });
        Ok(())
    }

    async fn cancel_card(&self, card_id: &str) {
        let task = self.tasks.lock().await.remove(card_id);
        if let Some(task) = task {
            task.stop.cancel();
        }
    }
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
        provider: WorkerProviderKind::Codex,
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
