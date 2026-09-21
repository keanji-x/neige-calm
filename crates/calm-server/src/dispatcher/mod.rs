//! Dispatcher worker: subscribes to task, report, hook, plan, and track events
//! that drive planner-harness push observations and scheduler pokes.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::db::{Repo, RouteRepo};
use crate::event::{
    BroadcastEnvelope, EditAuthor, Event, EventBus, SubscribeFilter, SubscribeScope,
};
use crate::event_cursor::EventCursorCache;
use crate::harness::{
    HarnessRegistry, HookKind as HarnessHookKind, Observation as HarnessObservation, PushLockGuard,
    is_harness_snapshot_value,
};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::CardRole;
use crate::operation::child_track_adapter::ChildTrackAdapter;
use crate::operation::claude_adapter::{ClaudeAdapter, ClaudeWorkerAdapter};
use crate::operation::claude_restart_adapter::ClaudeRestartAdapter;
use crate::operation::codex_adapter::{CodexAdapter, CodexWorkerAdapter};
use crate::operation::planner_harness_interrupt_adapter::PlannerHarnessInterruptAdapter;
use crate::operation::planner_harness_shutdown_adapter::PlannerHarnessShutdownAdapter;
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartAdapter;
use crate::operation::terminal_adapter::{TerminalAdapter, TerminalWorkerAdapter};
use crate::operation::{OperationCompletionBus, OperationRuntime, SpawnCtx, SqlxOperationRepo};
use crate::pending_codex_threads::PendingThreadStartRegistry;
use crate::plugin_host::{PluginHost, PluginRegistry};
use crate::provider_registry::WorkerProviderRegistry;
use crate::reaper::{DEFAULT_REAPER_RECONCILE_SECS, Reaper, reaper_disabled_from_env};
use crate::scheduler::{DEFAULT_RECONCILE_SECS, Scheduler, TerminalTaskHook};
use crate::session_projection_repo::WorkerSessionKind;
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::state::{CodexClient, DaemonClient, WriteContext};
use crate::task_context::TaskContextMonitor;
use crate::terminal_renderer::TerminalRendererRegistry;
use calm_types::git_candidate::DeliveryWakeReason;
use sha2::{Digest, Sha256};

pub(crate) use crate::db::sqlite::card_with_terminal_rollback_tx;

/// Default number of permits when `NEIGE_DISPATCHER_PERMITS` is unset / invalid / `0`.
const DEFAULT_PERMITS: usize = 8;

/// Report-edit authors that wake the planner; the planner system prompt renders this same list.
/// Planner/Kernel authors are absent — pushing their own edits back would loop.
pub(crate) const PLANNER_WAKE_AUTHORS: &[EditAuthor] =
    &[EditAuthor::User, EditAuthor::Plugin, EditAuthor::Assistant];

fn supervisor_sock_for_provider_registry(daemon: &DaemonClient) -> PathBuf {
    daemon
        .proc_supervisor_sock
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("neige-reaper-missing-proc-supervisor.sock"))
}
/// The event kinds `event_warrants_planner_push_with_role` can answer `true` for — exactly the
/// rows the boot catch-up reads back from the events table.
pub(crate) const PLANNER_CATCH_UP_KINDS: &[&str] = &[
    "task.completed",
    "task.failed",
    "task.execution_settled",
    "task.file_publication_settled",
    "task.candidate_verification_settled",
    "task.git_delivery_settled",
    "task.gate_result",
    "track.report_edited",
    "forge.scan.completed",
    "forge.pr.opened",
    "forge.pr.checks",
    "forge.issue.closed",
    "forge.pr.merged",
    "ratify.requested",
    "ratify.resolved",
    "codex.hook",
    "claude.hook",
];

/// Subscribed kinds that only poke the plan scheduler or the task-context monitor;
/// disjoint from `PLANNER_CATCH_UP_KINDS`.
pub(crate) const SCHEDULER_TRIGGER_KINDS: &[&str] = &[
    "plan.updated",
    "track.lifecycle_changed",
    "track.updated",
    "track.deleted",
    "area.deleted",
];

/// The one kind list the dispatcher's `SubscribeFilter` is built from.
pub(crate) fn dispatcher_subscription_kinds() -> Vec<String> {
    PLANNER_CATCH_UP_KINDS
        .iter()
        .chain(SCHEDULER_TRIGGER_KINDS.iter())
        .map(|kind| (*kind).to_string())
        .collect()
}

pub(crate) fn event_warrants_planner_push(
    event: &Event,
    actor: &ActorId,
    write: &WriteContext,
) -> bool {
    event_warrants_planner_push_with_role(event, actor, |card_id| write.verify_role(card_id))
}

pub(crate) fn event_warrants_planner_push_with_role(
    event: &Event,
    actor: &ActorId,
    mut role_for_card: impl FnMut(&CardId) -> Option<CardRole>,
) -> bool {
    match event {
        Event::TaskCompleted { .. } | Event::TaskFailed { .. } => {
            !crate::track_lifecycle::actor_is_planner_author(actor)
        }
        // Kernel-only at the role gate (no self-push loop); for a gated task this wake
        // replaces the suppressed worker self-report.
        Event::TaskGateResult { .. } => true,
        Event::TaskExecutionSettled { .. }
        | Event::TaskCandidateVerificationSettled { .. }
        | Event::TaskFilePublicationSettled { .. } => {
            matches!(actor, ActorId::Kernel | ActorId::KernelDispatcher)
        }
        // #1727 S4: pure on the event — the wake disposition was decided once in the settlement
        // tx; reading the tasks row here would let live push and boot replay disagree.
        Event::TaskGitDeliverySettled { wake_reason, .. } => {
            matches!(actor, ActorId::Kernel | ActorId::KernelDispatcher)
                && *wake_reason != DeliveryWakeReason::DeferredToGate
        }
        // User/Plugin/Assistant edits were not authored by the planner, so no self-push loop;
        // Planner/Kernel authors would loop.
        Event::TrackReportEdited { author, .. } => PLANNER_WAKE_AUTHORS.contains(author),
        Event::ForgePrMerged { .. }
        | Event::RatifyRequested { .. }
        | Event::RatifyResolved { .. }
        | Event::ForgeScanCompleted { .. }
        | Event::ForgePrOpened { .. }
        | Event::ForgePrChecks { .. }
        | Event::ForgeIssueClosed { .. } => true,
        // Workspace / worktree lifecycle notices are read back on demand (`calm.plan.list`);
        // `review.round` is planner-authored, so pushing it would be self-echo.
        Event::WorkspaceLeased { .. }
        | Event::WorkspaceReleased { .. }
        | Event::WorktreeProvisioned { .. }
        | Event::WorktreeCommitted { .. }
        | Event::ReviewRound { .. } => false,
        Event::CodexHook { card_id, kind, .. } | Event::ClaudeHook { card_id, kind, .. } => {
            let is_turn_end = kind == "hook.codex.stop" || kind == "hook.claude.stop";
            let is_worker = role_for_card(card_id) == Some(CardRole::Worker);
            is_turn_end && is_worker
        }
        Event::AreaUpdated(_)
        | Event::AreaDeleted { .. }
        | Event::TrackUpdated(_)
        | Event::TrackDeleted { .. }
        | Event::TrackLifecycleChanged { .. }
        | Event::CardAdded(_)
        | Event::CardUpdated(_)
        | Event::CardDeleted { .. }
        | Event::WorkerSessionStarted { .. }
        | Event::WorkerSessionStatusChanged { .. }
        | Event::WorkerSessionSuperseded { .. }
        | Event::HarnessItemAdded { .. }
        | Event::HarnessPhaseChanged { .. }
        | Event::HarnessTranscriptCleared { .. }
        | Event::HarnessUserMessageEnqueued { .. }
        | Event::HarnessQueueChanged { .. }
        | Event::OverlaySet(_)
        | Event::OverlayDeleted { .. }
        | Event::TerminalDeleted { .. }
        | Event::PluginState { .. }
        | Event::PluginToolRegistered { .. }
        | Event::CodexWorkerRequested { .. }
        | Event::TerminalWorkerRequested { .. }
        | Event::PlanUpdated { .. }
        | Event::TaskDispatched { .. }
        | Event::TaskContextFrozen { .. }
        | Event::TaskContextAdvanced { .. }
        | Event::ForgePrDiffRead { .. }
        | Event::ForgeIssueRead { .. }
        | Event::ProposalSubmitted { .. }
        | Event::ProposalResolved { .. }
        | Event::WorktreeRemoved { .. } => false,
    }
}

/// A worker self-report whose wake is deferred to a later kernel event is not pushed: a
/// `task.completed` whose attempt has a `task_git_deliveries` row (the kernel delivery settles it,
/// `task.git_delivery_settled` wakes the planner; the row is written in the report transaction and
/// never deleted below a Track, so live push and replay agree), and a report for a tasks row with
/// `gate_json` set (the planner hears `task.gate_result` instead). Not status-based: a fast gate can
/// flip the row terminal before this read. A gated `task.failed` is pushed only when the failure
/// landed pre-gate; lookup errors fail open.
pub(crate) async fn is_deferred_self_report(repo: &dyn crate::db::Repo, event: &Event) -> bool {
    let (idempotency_key, is_failure) = match event {
        Event::TaskCompleted {
            idempotency_key, ..
        } => (idempotency_key, false),
        Event::TaskFailed {
            idempotency_key, ..
        } => (idempotency_key, true),
        _ => return false,
    };
    if !is_failure && let Some(pool) = repo.sqlite_pool() {
        match crate::git_candidate::delivery::attempt_has_delivery(&pool, idempotency_key).await {
            Ok(true) => return true,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(idempotency_key = %idempotency_key, error = %e, "dispatcher push: delivery-row lookup failed; consulting the gate rule (fail-open)")
            }
        }
    }
    match repo.task_get(idempotency_key).await {
        Ok(Some(task)) => {
            if task.gate_json.is_none() {
                return false;
            }
            if !is_failure {
                return true;
            }
            let failure_landed_pre_gate = task.status == crate::model::TaskStatus::Failed
                && matches!(
                    task.status_detail
                        .as_deref()
                        .map(crate::db::sqlite::status_detail_class),
                    Some("worker-reported") | Some("spawn-failed") | Some("worker-timeout")
                );
            !failure_landed_pre_gate
        }
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(
                idempotency_key = %idempotency_key,
                error = %e,
                "dispatcher push: gated-self-report lookup failed; pushing self-report (fail-open)"
            );
            false
        }
    }
}

/// A worker stop hook is a wake only while its tasks row is still `dispatched | running`; past that
/// the gate result / terminal event is the wake. Returns `true` when the push must be suppressed;
/// no tasks row or a lookup error → push (fail-open).
pub(crate) async fn is_stale_worker_stop_hook(repo: &dyn crate::db::Repo, event: &Event) -> bool {
    let card_id = match event {
        Event::CodexHook { card_id, .. } | Event::ClaudeHook { card_id, .. } => card_id,
        _ => return false,
    };
    match repo.task_for_worker_card(card_id.as_str()).await {
        Ok(Some(task)) => !matches!(
            task.status,
            crate::model::TaskStatus::Dispatched | crate::model::TaskStatus::Running
        ),
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(
                card_id = %card_id,
                error = %e,
                "dispatcher push: stale-worker-stop lookup failed; pushing stop hook (fail-open)"
            );
            false
        }
    }
}

fn empty_plugin_host_for_dispatcher_runtime(
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
) -> Arc<PluginHost> {
    let route_repo: Arc<dyn RouteRepo> = repo;
    Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        route_repo,
        PathBuf::new(),
        std::env::temp_dir().join("calm-dispatcher-plugins-data"),
        Vec::new(),
        events,
        write,
    ))
}

#[allow(deprecated, clippy::too_many_arguments)]
fn dispatcher_operation_runtime(
    repo: Arc<dyn Repo>,
    events: EventBus,
    write: WriteContext,
    codex: Arc<CodexClient>,
    daemon: Arc<DaemonClient>,
    terminal_renderer: Arc<TerminalRendererRegistry>,
    mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
    shared_codex_appserver: Arc<SharedCodexAppServer>,
    harness: HarnessRegistry,
    plugin: Arc<PluginHost>,
    workspace_root: std::path::PathBuf,
) -> Arc<OperationRuntime> {
    let route_repo: Arc<dyn RouteRepo> = repo.clone();
    let operation_repo = Arc::new(SqlxOperationRepo::new(
        repo.sqlite_pool()
            .expect("Dispatcher operation runtime requires a sqlite-backed Repo"),
    ));
    let pending_codex_threads = Arc::new(PendingThreadStartRegistry::new(
        repo.clone(),
        events.clone(),
    ));
    let pending_codex_threads_spawn_serial = Arc::new(tokio::sync::Mutex::new(()));
    let terminal_adapter = Arc::new(TerminalAdapter::new(
        route_repo.clone(),
        write.role_cache().clone(),
        write.area_cache().clone(),
    ));
    let terminal_worker_adapter = Arc::new(TerminalWorkerAdapter::new(
        route_repo.clone(),
        write.role_cache().clone(),
        write.area_cache().clone(),
    ));
    let codex_adapter = Arc::new(CodexAdapter::new(
        route_repo.clone(),
        codex.clone(),
        shared_codex_appserver.clone(),
        pending_codex_threads.clone(),
        pending_codex_threads_spawn_serial,
        write.role_cache().clone(),
        write.area_cache().clone(),
    ));
    let mcp_socket_path = mcp_server
        .as_ref()
        .map(|s| s.shim_config.socket_path.clone());
    let isolated_codex_adapter =
        Arc::new(crate::isolated_codex::adapter::IsolatedCodexAdapter::new(
            None,
            route_repo.clone(),
            mcp_socket_path.clone(),
            write.clone(),
        ));
    let codex_worker_adapter = Arc::new(CodexWorkerAdapter::new(
        route_repo.clone(),
        codex.clone(),
        shared_codex_appserver.clone(),
        mcp_server.clone(),
        write.role_cache().clone(),
        write.area_cache().clone(),
        workspace_root.clone(),
    ));
    let claude_adapter = Arc::new(ClaudeAdapter::new(
        route_repo.clone(),
        codex.clone(),
        write.role_cache().clone(),
        write.area_cache().clone(),
    ));
    let claude_worker_adapter = Arc::new(ClaudeWorkerAdapter::new(
        route_repo.clone(),
        codex.clone(),
        mcp_server.clone(),
        write.role_cache().clone(),
        write.area_cache().clone(),
        workspace_root.clone(),
    ));
    let claude_restart_adapter = Arc::new(ClaudeRestartAdapter::new(
        route_repo.clone(),
        codex,
        write.role_cache().clone(),
        write.area_cache().clone(),
    ));
    let planner_harness_start_adapter = Arc::new(PlannerHarnessStartAdapter::new(
        repo.clone(),
        shared_codex_appserver.clone(),
        harness.clone(),
        plugin,
        write.role_cache().clone(),
        write.area_cache().clone(),
        mcp_socket_path,
    ));
    let planner_harness_interrupt_adapter =
        Arc::new(PlannerHarnessInterruptAdapter::new(harness.clone()));
    let planner_harness_shutdown_adapter = Arc::new(PlannerHarnessShutdownAdapter::new(
        harness,
        shared_codex_appserver.clone(),
        repo,
    ));
    let task_verify_adapter = Arc::new(
        crate::operation::task_verify_adapter::TaskVerifyAdapter::new(
            crate::operation::task_verify_adapter::TaskVerifyAdapter::default_gate_logs_dir(),
        ),
    );
    let forge_action_adapter =
        Arc::new(crate::operation::forge_action_adapter::ForgeActionAdapter::new());
    let child_track_adapter = Arc::new(ChildTrackAdapter::new(
        write.role_cache().clone(),
        write.area_cache().clone(),
        workspace_root.clone(),
    ));
    let completion = OperationCompletionBus::new();
    Arc::new(OperationRuntime::new_unchecked(
        operation_repo.clone(),
        vec![
            terminal_adapter,
            terminal_worker_adapter,
            codex_adapter,
            codex_worker_adapter,
            isolated_codex_adapter,
            Arc::new(crate::file_delivery::adapter::FilePublicationAdapter::new(
                route_repo.clone(),
            )),
            Arc::new(
                crate::file_delivery::candidate_verify::CandidateVerifyAdapter::new(
                    route_repo.clone(),
                ),
            ),
            claude_adapter,
            claude_worker_adapter,
            claude_restart_adapter,
            planner_harness_start_adapter,
            planner_harness_interrupt_adapter,
            planner_harness_shutdown_adapter,
            task_verify_adapter,
            forge_action_adapter,
            child_track_adapter,
        ],
        events.clone(),
        completion.clone(),
        SpawnCtx::new(
            route_repo,
            operation_repo,
            daemon,
            terminal_renderer,
            events,
            completion,
        )
        .with_shared_codex_appserver(shared_codex_appserver.clone()),
    ))
}

/// Suspend one real failure handler before its push lock, without delaying cleanup.
#[cfg(any(test, feature = "fixtures"))]
pub struct TaskFailurePushTestHook {
    pub task_id: String,
    pub entered: Arc<tokio::sync::Notify>,
    pub resume: Arc<tokio::sync::Notify>,
    pub finished: Arc<tokio::sync::Notify>,
}

/// Subscribed handle. Holding the [`Dispatcher`] keeps the spawned task alive; dropping it
/// closes the broadcast receiver's end.
pub struct Dispatcher {
    semaphore: Arc<Semaphore>,
    permits: usize,
    #[allow(dead_code)]
    handle: JoinHandle<()>,
    inner: Arc<Inner>,
    /// The background task only keeps a `Weak`; this keeps the runtime alive while the handle lives.
    #[allow(dead_code)]
    operation_runtime: Arc<OperationRuntime>,
    scheduler: Arc<Scheduler>,
    context_monitor: Arc<TaskContextMonitor>,
    /// Slow periodic reconcile sweep (`NEIGE_SCHEDULER_RECONCILE_SECS`, default 300).
    #[allow(dead_code)]
    reconcile_handle: JoinHandle<()>,
    /// `None` when `NEIGE_REAPER_DISABLED` is set.
    #[allow(dead_code)]
    reaper_handle: Option<JoinHandle<()>>,
    /// Durable codex worker-liveness feeder; `None` when the reaper is disabled.
    #[allow(dead_code)]
    liveness_feeder_handle: Option<JoinHandle<()>>,
}

impl Dispatcher {
    /// Permit count from `NEIGE_DISPATCHER_PERMITS`, falling back to `default` when unset,
    /// unparseable, or zero.
    pub fn permits_from_env(default: usize) -> usize {
        match std::env::var("NEIGE_DISPATCHER_PERMITS") {
            Ok(raw) => match raw.trim().parse::<usize>() {
                Ok(n) if n > 0 => n,
                _ => default,
            },
            Err(_) => default,
        }
    }

    pub fn permits(&self) -> usize {
        self.permits
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn set_task_failure_push_hook_for_test(&self, hook: TaskFailurePushTestHook) {
        *self
            .inner
            .failure_push_hook
            .lock()
            .expect("failure push hook") = Some(hook);
    }

    /// Test-only — read the current in-memory push cursor for a card.
    #[doc(hidden)]
    pub fn push_cursor_for_test(&self, planner_card_id: &CardId) -> i64 {
        self.inner.push_cursor.get(planner_card_id)
    }

    /// Replay an already-persisted `(envelope_id, scope, event)` through the push path without the
    /// broadcast bus. `envelope_id` must be the real persisted `events.id` — the watermark dedup keys on it.
    pub async fn catch_up_push(
        &self,
        track_id: TrackId,
        event: crate::event::Event,
        envelope_id: i64,
    ) {
        Inner::observe_harness(&self.inner, track_id, &event, envelope_id).await;
    }

    pub fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.semaphore)
    }

    pub fn scheduler(&self) -> Arc<Scheduler> {
        Arc::clone(&self.scheduler)
    }

    pub fn context_monitor(&self) -> Arc<TaskContextMonitor> {
        Arc::clone(&self.context_monitor)
    }

    /// Stops only the event listener so `PlanUpdated` cannot race a fixture's next request.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn abort_event_listener_for_test(&self) {
        self.handle.abort();
    }

    /// Called only while assembling a fixture, before work is submitted.
    #[cfg(feature = "fixtures")]
    pub(crate) fn stop_background_for_fixture_rebuild(&self) {
        self.handle.abort();
        self.reconcile_handle.abort();
        if let Some(handle) = &self.reaper_handle {
            handle.abort();
        }
        if let Some(handle) = &self.liveness_feeder_handle {
            handle.abort();
        }
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub async fn reconcile_tick_for_test(&self) {
        self.inner.reconcile_once().await;
    }

    /// Spawn the dispatcher background task. Production passes
    /// `permits_from_env(DEFAULT_PERMITS)`; tests inject an explicit count.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        workspace_root: std::path::PathBuf,
        permits: usize,
    ) -> Self {
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo);
        Self::spawn_with_terminal_renderer(
            repo,
            events,
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            shared_codex_appserver,
            workspace_root,
            permits,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_operation_runtime(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        operation_runtime: Arc<OperationRuntime>,
        permits: usize,
    ) -> Self {
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let terminal_renderer = TerminalRendererRegistry::new_with_repo(route_repo);
        Self::spawn_with_terminal_renderer_and_operation_runtime(
            repo,
            events,
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            shared_codex_appserver,
            operation_runtime,
            permits,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_terminal_renderer(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        terminal_renderer: Arc<TerminalRendererRegistry>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        workspace_root: std::path::PathBuf,
        permits: usize,
    ) -> Self {
        let plugin =
            empty_plugin_host_for_dispatcher_runtime(repo.clone(), events.clone(), write.clone());
        let operation_runtime = dispatcher_operation_runtime(
            repo.clone(),
            events.clone(),
            write.clone(),
            codex.clone(),
            daemon.clone(),
            terminal_renderer.clone(),
            mcp_server.clone(),
            shared_codex_appserver.clone(),
            HarnessRegistry::new(),
            plugin,
            workspace_root,
        );
        Self::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
            repo,
            events,
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            HarnessRegistry::new(),
            shared_codex_appserver,
            operation_runtime,
            permits,
            Scheduler::budget_from_env(crate::scheduler::DEFAULT_TRACK_TASK_BUDGET),
            crate::operation::task_verify_adapter::TaskVerifyAdapter::default_gate_logs_dir(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_terminal_renderer_and_operation_runtime(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        terminal_renderer: Arc<TerminalRendererRegistry>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        operation_runtime: Arc<OperationRuntime>,
        permits: usize,
    ) -> Self {
        Self::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
            repo,
            events,
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            HarnessRegistry::new(),
            shared_codex_appserver,
            operation_runtime,
            permits,
            Scheduler::budget_from_env(crate::scheduler::DEFAULT_TRACK_TASK_BUDGET),
            crate::operation::task_verify_adapter::TaskVerifyAdapter::default_gate_logs_dir(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_terminal_renderer_and_harness(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        terminal_renderer: Arc<TerminalRendererRegistry>,
        mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        harness: HarnessRegistry,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        workspace_root: std::path::PathBuf,
        permits: usize,
    ) -> Self {
        let plugin =
            empty_plugin_host_for_dispatcher_runtime(repo.clone(), events.clone(), write.clone());
        let operation_runtime = dispatcher_operation_runtime(
            repo.clone(),
            events.clone(),
            write.clone(),
            codex.clone(),
            daemon.clone(),
            terminal_renderer.clone(),
            mcp_server.clone(),
            shared_codex_appserver.clone(),
            harness.clone(),
            plugin,
            workspace_root,
        );
        Self::spawn_with_terminal_renderer_and_harness_and_operation_runtime(
            repo,
            events,
            write,
            codex,
            daemon,
            terminal_renderer,
            mcp_server,
            harness,
            shared_codex_appserver,
            operation_runtime,
            permits,
            Scheduler::budget_from_env(crate::scheduler::DEFAULT_TRACK_TASK_BUDGET),
            crate::operation::task_verify_adapter::TaskVerifyAdapter::default_gate_logs_dir(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_terminal_renderer_and_harness_and_operation_runtime(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        _codex: Arc<CodexClient>,
        daemon: Arc<DaemonClient>,
        terminal_renderer: Arc<TerminalRendererRegistry>,
        _mcp_server: Option<Arc<crate::mcp_server::McpServer>>,
        harness: HarnessRegistry,
        shared_codex_appserver: Arc<SharedCodexAppServer>,
        operation_runtime: Arc<OperationRuntime>,
        permits: usize,
        task_budget_default: i64,
        gate_logs_dir: PathBuf,
    ) -> Self {
        let permits = if permits == 0 {
            DEFAULT_PERMITS
        } else {
            permits
        };
        let semaphore = Arc::new(Semaphore::new(permits));
        let scheduler = Scheduler::new_with_task_budget_default(
            repo.clone(),
            events.clone(),
            write.clone(),
            Arc::downgrade(&operation_runtime),
            Arc::clone(&semaphore),
            gate_logs_dir,
            task_budget_default,
        );
        let context_monitor = Arc::new(TaskContextMonitor::new_with_metrics(
            repo.clone(),
            events.clone(),
            write.clone(),
            scheduler.context_metrics(),
        ));
        // Take the feeder's notification subscription BEFORE `shared_codex_appserver` is moved
        // into the provider registry.
        let liveness_feeder_rx = shared_codex_appserver.subscribe_notifications();
        let liveness_feeder_repo = repo.clone();
        let provider_registry = WorkerProviderRegistry::new(
            supervisor_sock_for_provider_registry(&daemon),
            shared_codex_appserver,
        );
        let reaper = Arc::new(Reaper::new(
            repo.clone(),
            provider_registry,
            events.clone(),
            write.clone(),
        ));
        terminal_renderer.set_task_hook(TerminalTaskHook::new(
            repo.clone(),
            events.clone(),
            write.clone(),
        ));
        let inner = Arc::new(Inner {
            repo,
            write,
            harness,
            scheduler: Arc::clone(&scheduler),
            context_monitor: Arc::clone(&context_monitor),
            // A push only fires when `envelope_id > cursor`, making pushes idempotent under
            // at-least-once delivery.
            push_cursor: EventCursorCache::new(),
            push_locks: DashMap::new(),
            #[cfg(any(test, feature = "fixtures"))]
            failure_push_hook: std::sync::Mutex::new(None),
            semaphore: Arc::clone(&semaphore),
        });

        // Hook events are coarse-filtered by `kind_tag()` here; the exact turn-ending hook
        // discriminators are checked in the push branch.
        let filter = SubscribeFilter {
            scope: SubscribeScope::Any,
            include_descendants: true,
            kinds: Some(dispatcher_subscription_kinds()),
        };
        let mut rx = events.subscribe_filtered();

        let inner_for_task = Arc::clone(&inner);
        let filter_for_task = filter.clone();
        let handle = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(envelope) => {
                        // `subscribe_filtered` hands back the raw firehose; callers run the match themselves.
                        if !filter_for_task.matches(&envelope) {
                            continue;
                        }
                        let inner = Arc::clone(&inner_for_task);
                        // Per-event spawn is fire-and-forget: the bus
                        // reader keeps draining while the
                        // semaphore-gated handler is in flight.
                        tokio::spawn(async move {
                            inner.handle_envelope(envelope).await;
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            skipped = n,
                            "dispatcher subscriber lagged; missed events may need a retry from the requester"
                        );
                        // A lagged `plan.updated` / `task.completed` would strand pending tasks until the next
                        // reconcile tick — sweep now. `sweep_all` is boot-gated, so a lag during boot no-ops here.
                        let scheduler = Arc::clone(&inner_for_task.scheduler);
                        let context_monitor = Arc::clone(&inner_for_task.context_monitor);
                        tokio::spawn(async move {
                            if let Err(error) = context_monitor.sweep().await {
                                tracing::warn!(%error, "task context sweep after lag failed");
                            } else {
                                scheduler.open_context_sweep_gate().await;
                            }
                            scheduler.sweep_all().await;
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Slow reconcile tick; correctness never depends on it, it restores liveness after a lost envelope.
        let tick_inner = Arc::clone(&inner);
        let reconcile_handle = tokio::spawn(async move {
            let period = std::time::Duration::from_secs(Scheduler::reconcile_secs_from_env(
                DEFAULT_RECONCILE_SECS,
            ));
            let mut interval = tokio::time::interval(period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick fires immediately; skip it — boot runs its own sweep, and `sweep_all`'s
            // boot gate covers later ticks that beat the boot funnel.
            interval.tick().await;
            loop {
                interval.tick().await;
                tick_inner.reconcile_once().await;
            }
        });
        let reaper_handle = if reaper_disabled_from_env() {
            None
        } else {
            let tick_reaper = Arc::clone(&reaper);
            Some(tokio::spawn(async move {
                let period =
                    std::time::Duration::from_secs(Scheduler::reconcile_secs_from_env_var(
                        "NEIGE_REAPER_RECONCILE_SECS",
                        DEFAULT_REAPER_RECONCILE_SECS,
                    ));
                let mut interval = tokio::time::interval(period);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // The first tick fires immediately; skip it. The reaper
                // has its own boot gate and remains observational after it opens.
                interval.tick().await;
                loop {
                    interval.tick().await;
                    tick_reaper.sweep_all().await;
                    tick_reaper.sweep_dead_roots().await;
                }
            }))
        };

        // Gated behind the same kill-switch as the reaper: nothing consumes its writes otherwise.
        let liveness_feeder_handle = if reaper_disabled_from_env() {
            None
        } else {
            Some(crate::liveness_feeder::spawn_liveness_feeder(
                liveness_feeder_repo,
                liveness_feeder_rx,
            ))
        };

        Self {
            semaphore,
            permits,
            handle,
            inner,
            operation_runtime,
            scheduler,
            context_monitor,
            reconcile_handle,
            reaper_handle,
            liveness_feeder_handle,
        }
    }
}

struct Inner {
    repo: Arc<dyn Repo>,
    write: WriteContext,
    harness: HarnessRegistry,
    scheduler: Arc<Scheduler>,
    context_monitor: Arc<TaskContextMonitor>,
    /// A push fires only when `envelope_id > cursor`, making pushes idempotent under
    /// at-least-once broadcast delivery.
    push_cursor: EventCursorCache,
    /// Serialize cursor/enqueue updates. Lock acquisition does not order
    /// separately spawned handlers by event ID. Settlement catches up its
    /// persisted prefix before advancing past a delayed failure handler.
    push_locks: DashMap<TrackId, Arc<tokio::sync::Mutex<()>>>,
    #[cfg(any(test, feature = "fixtures"))]
    failure_push_hook: std::sync::Mutex<Option<TaskFailurePushTestHook>>,
    semaphore: Arc<Semaphore>,
}

impl Inner {
    /// The periodic loop and scheduler tests share this exact body.
    async fn reconcile_once(&self) {
        if let Err(error) = self.context_monitor.sweep().await {
            tracing::warn!(%error, "periodic task context sweep failed");
            self.scheduler.sweep_all().await;
        } else if !self.scheduler.open_context_sweep_gate().await {
            self.scheduler.sweep_all().await;
        }
    }

    async fn handle_envelope(self: Arc<Self>, envelope: BroadcastEnvelope) {
        let _permit = match Arc::clone(&self.semaphore).acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("dispatcher semaphore closed; aborting spawn");
                return;
            }
        };

        #[cfg(any(test, feature = "fixtures"))]
        let failure_push_hook = {
            let mut pending = self.failure_push_hook.lock().expect("failure push hook");
            if matches!(&envelope.event, Event::TaskFailed { idempotency_key, .. }
                if pending.as_ref().is_some_and(|hook| &hook.task_id == idempotency_key))
            {
                pending.take()
            } else {
                None
            }
        };
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(hook) = &failure_push_hook {
            hook.entered.notify_one();
            hook.resume.notified().await;
        }

        // Push branch. Planner/Kernel-authored report edits are the planner writing its own
        // report; pushing them back would loop.
        match &envelope.event {
            Event::TaskCompleted { .. }
            | Event::TaskFailed { .. }
            | Event::TaskGateResult { .. }
            | Event::TaskGitDeliverySettled { .. }
            | Event::TaskExecutionSettled { .. } | Event::TaskCandidateVerificationSettled { .. } | Event::TaskFilePublicationSettled { .. } => {
                if event_warrants_planner_push(&envelope.event, &envelope.actor, &self.write)
                    && !is_deferred_self_report(self.repo.as_ref(), &envelope.event).await
                {
                    if let Some(track_id) = envelope.scope.track_id().cloned() {
                        self.observe_harness(track_id, &envelope.event, envelope.id)
                            .await;
                    } else {
                        tracing::debug!(
                            kind = envelope.event.kind_tag(),
                            "dispatcher push: task event has no track scope; skipping"
                        );
                    }
                }
                // A task terminal event may free budget / satisfy deps; poke the scheduler AFTER the push branch.
                if let Some(track_id) = envelope.scope.track_id().cloned() {
                    self.scheduler.poke(track_id);
                }
            }
            Event::PlanUpdated { track_id, .. } => {
                self.scheduler.poke(track_id.clone());
            }
            Event::TrackLifecycleChanged { id, .. } => {
                self.scheduler.reconcile_child_track(id.clone());
                self.scheduler.poke(id.clone());
            }
            // `PATCH /api/tracks` emits only `track.updated` when it changes `task_budget`; without
            // this arm a raised budget would strand pending tasks until the reconcile tick.
            Event::TrackUpdated(payload) => {
                self.scheduler.poke(payload.id.clone());
            }
            Event::TrackReportEdited {
                author, track_id, ..
            } => {
                // Mechanical invalidation is independent of author and runs before the self-push suppression.
                let context_monitor = Arc::clone(&self.context_monitor);
                let detection_track_id = track_id.clone();
                tokio::spawn(async move {
                    if let Err(error) = context_monitor
                        .detect_track_edit(detection_track_id.as_str())
                        .await
                    {
                        tracing::warn!(%error, track_id = %detection_track_id, "task context edit detection failed");
                    }
                });
                if event_warrants_planner_push(&envelope.event, &envelope.actor, &self.write) {
                    self.observe_harness(track_id.clone(), &envelope.event, envelope.id)
                        .await;
                } else {
                    tracing::trace!(
                        ?author,
                        "dispatcher push: ignoring planner/kernel-authored track.report_edited"
                    );
                }
            }
            Event::TrackDeleted { id, .. } => {
                self.scheduler.reconcile_child_track(id.clone());
                let context_monitor = Arc::clone(&self.context_monitor);
                tokio::spawn(async move {
                    if let Err(error) = context_monitor.sweep().await {
                        tracing::warn!(%error, "task context deletion sweep failed");
                    }
                });
            }
            Event::AreaDeleted { .. } => {
                let context_monitor = Arc::clone(&self.context_monitor);
                tokio::spawn(async move {
                    if let Err(error) = context_monitor.sweep().await {
                        tracing::warn!(%error, "task context deletion sweep failed");
                    }
                });
            }
            Event::ForgePrMerged { track_id, .. }
            | Event::RatifyRequested { track_id, .. }
            | Event::RatifyResolved { track_id, .. }
            | Event::ForgeScanCompleted { track_id, .. }
            | Event::ForgePrOpened { track_id, .. }
            | Event::ForgePrChecks { track_id, .. }
            | Event::ForgeIssueClosed { track_id, .. } => {
                if event_warrants_planner_push(&envelope.event, &envelope.actor, &self.write) {
                    self.observe_harness(track_id.clone(), &envelope.event, envelope.id)
                        .await;
                }
            }
            Event::CodexHook { card_id, kind, .. } | Event::ClaudeHook { card_id, kind, .. } => {
                // Only the precise Stop hooks end a worker turn; other hooks are mid-turn pauses. The
                // Worker role gate prevents planner self-push loops.
                if event_warrants_planner_push(&envelope.event, &envelope.actor, &self.write)
                    && !is_stale_worker_stop_hook(self.repo.as_ref(), &envelope.event).await
                {
                    if let Some(track_id) = envelope.scope.track_id().cloned() {
                        self.observe_harness(track_id, &envelope.event, envelope.id)
                            .await;
                    } else {
                        tracing::debug!(
                            kind = envelope.event.kind_tag(),
                            hook_kind = %kind,
                            card_id = %card_id,
                            "dispatcher push: worker hook stop has no track scope; skipping"
                        );
                    }
                } else {
                    tracing::trace!(
                        hook_kind = %kind,
                        card_id = %card_id,
                        "dispatcher push: ignoring hook event"
                    );
                }
            }
            Event::AreaUpdated(_)
            | Event::CardAdded(_)
            | Event::CardUpdated(_)
            | Event::CardDeleted { .. }
            | Event::WorkerSessionStarted { .. }
            | Event::WorkerSessionStatusChanged { .. }
            | Event::WorkerSessionSuperseded { .. }
            | Event::HarnessItemAdded { .. }
            | Event::HarnessPhaseChanged { .. }
            | Event::HarnessTranscriptCleared { .. }
            | Event::HarnessUserMessageEnqueued { .. }
            | Event::HarnessQueueChanged { .. }
            | Event::OverlaySet(_)
            | Event::OverlayDeleted { .. }
            | Event::TerminalDeleted { .. }
            | Event::PluginState { .. }
            | Event::PluginToolRegistered { .. }
            | Event::CodexWorkerRequested { .. }
            | Event::TerminalWorkerRequested { .. }
            | Event::TaskDispatched { .. }
            | Event::TaskContextFrozen { .. }
            | Event::TaskContextAdvanced { .. }
            | Event::ForgePrDiffRead { .. }
            | Event::ForgeIssueRead { .. }
            // Proposal lifecycle events reach the planner via the plugin-authored
            // `track.report_edited` landed in the same tx.
            | Event::ProposalSubmitted { .. }
            | Event::ProposalResolved { .. }
            | Event::WorkspaceLeased { .. }
            | Event::WorkspaceReleased { .. }
            | Event::WorktreeProvisioned { .. }
            | Event::WorktreeCommitted { .. }
            | Event::ReviewRound { .. }
            | Event::WorktreeRemoved { .. } => {
                tracing::warn!(
                    kind = envelope.event.kind_tag(),
                    "dispatcher received event with no handler; filter widened unexpectedly",
                );
            }
        }
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(hook) = failure_push_hook {
            hook.finished.notify_one();
        }
    }

    async fn observe_harness(self: &Arc<Self>, track_id: TrackId, event: &Event, envelope_id: i64) {
        let guard = self.acquire_push_lock(&track_id).await;
        self.observe_harness_under_lock(&guard, event, envelope_id)
            .await;
    }

    /// Per-track push lock so same-track replay and live pushes serialize around `(get → compare → bump)`.
    async fn acquire_push_lock(self: &Arc<Self>, track_id: &TrackId) -> PushLockGuard {
        // IMPORTANT: do NOT bind the DashMap Entry to a `let` — the shard
        // guard must drop at this statement's `;` before we `.await` below.
        let lock = self
            .push_locks
            .entry(track_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let guard = lock.lock_owned().await;
        PushLockGuard::new(track_id.clone(), guard)
    }

    async fn observe_harness_under_lock(
        self: &Arc<Self>,
        guard: &PushLockGuard,
        event: &Event,
        envelope_id: i64,
    ) {
        let track_id = guard.track_id().clone();
        let planner_card_id = match self.resolve_planner_card(&track_id).await {
            Some(id) => id,
            None => {
                tracing::debug!(
                    track_id = %track_id,
                    "dispatcher push: no planner card found for track; skipping"
                );
                return;
            }
        };

        // A synthetic id-0 envelope (test `EventBus::emit`) is never above the initial 0 cursor, so it
        // is skipped; `bump` is monotonic, so a re-delivered id can't double-push.
        let cursor = self.push_cursor.get(&planner_card_id);
        if envelope_id <= cursor {
            tracing::debug!(
                track_id = %track_id,
                planner_card_id = %planner_card_id,
                envelope_id,
                cursor,
                "dispatcher push: envelope id not above watermark; deduped"
            );
            return;
        }

        let Some(runtime_id) = self
            .harness_runtime_id_for_planner_card(&planner_card_id)
            .await
        else {
            tracing::debug!(
                track_id = %track_id,
                planner_card_id = %planner_card_id,
                envelope_id,
                kind = event.kind_tag(),
                "dispatcher push: planner card has no harness runtime; skipping observation"
            );
            return;
        };
        let observation = match resolve_harness_observation(self.repo.as_ref(), &track_id, event)
            .await
        {
            Ok(observation) => observation,
            Err(error) => {
                tracing::warn!(%track_id, %error, "planner observation lookup failed; preserving cursor for replay");
                return;
            }
        };
        let Some(observation) = observation else {
            tracing::debug!(
                track_id = %track_id,
                planner_card_id = %planner_card_id,
                envelope_id,
                kind = event.kind_tag(),
                "dispatcher push: harness runtime found but event did not map to a harness observation"
            );
            return;
        };
        let Some(harness) = self.harness.get(&runtime_id) else {
            tracing::warn!(
                track_id = %track_id,
                planner_card_id = %planner_card_id,
                runtime_id = %runtime_id,
                envelope_id,
                kind = event.kind_tag(),
                "dispatcher push: no live PlannerHarness for harness runtime; cursor NOT bumped so snapshot recovery will replay on boot"
            );
            return;
        };
        // Recovery may have already accepted this prefix while the Dispatcher
        // cache is cold. Share that trusted floor with every mapped observation,
        // including a failure arriving before or after its settlement replay.
        let cursor = self.push_cursor.bump(
            planner_card_id.clone(),
            cursor.max(harness.snapshot().await.push_watermark),
        );
        if envelope_id <= cursor {
            return;
        }
        if matches!(
            event,
            Event::TaskExecutionSettled { .. }
                | Event::TaskCandidateVerificationSettled { .. }
                | Event::TaskGitDeliverySettled { .. }
                | Event::TaskFilePublicationSettled { .. }
        ) {
            let preceding = match crate::harness::catch_up::observations_since(
                self.repo.as_ref(),
                &track_id,
                cursor,
                Some(envelope_id - 1),
            )
            .await
            {
                Ok(observations) => observations,
                Err(error) => {
                    tracing::warn!(%track_id, %error, "settlement prefix lookup failed; preserving cursor for replay");
                    return;
                }
            };
            for (id, observation) in preceding {
                if let Err(error) = harness.observe_envelope(observation, id) {
                    tracing::warn!(%track_id, %error, "settlement prefix enqueue failed; preserving cursor for replay");
                    return;
                }
                self.push_cursor.bump(planner_card_id.clone(), id);
            }
        }
        tracing::info!(
            track_id = %track_id,
            planner_card_id = %planner_card_id,
            runtime_id = %runtime_id,
            envelope_id,
            kind = event.kind_tag(),
            "dispatcher push: delivering observation to planner harness"
        );
        if let Err(e) = harness.observe_envelope(observation, envelope_id) {
            tracing::warn!(
                track_id = %track_id,
                planner_card_id = %planner_card_id,
                runtime_id = %runtime_id,
                envelope_id,
                kind = event.kind_tag(),
                error = %e,
                "dispatcher push: PlannerHarness observation enqueue failed; cursor NOT bumped so snapshot recovery will replay on boot"
            );
            return;
        }
        self.push_cursor.bump(planner_card_id.clone(), envelope_id);
    }

    /// Find the planner card for a track via `card_role_cache`; `None` if the track has none or the lookup errors.
    async fn resolve_planner_card(self: &Arc<Self>, track_id: &TrackId) -> Option<CardId> {
        let cards = match self.repo.cards_by_track(track_id.as_str()).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    track_id = %track_id,
                    error = %e,
                    "dispatcher push: cards_by_track failed; cannot resolve planner card"
                );
                return None;
            }
        };
        cards.into_iter().find_map(|c| {
            if self.write.verify_role(&c.id) == Some(CardRole::Planner) {
                Some(c.id)
            } else {
                None
            }
        })
    }

    async fn harness_runtime_id_for_planner_card(
        self: &Arc<Self>,
        planner_card_id: &CardId,
    ) -> Option<String> {
        let runtime = match self
            .repo
            .session_projection_active_for_card(&planner_card_id.to_string())
            .await
        {
            Ok(runtime) => runtime?,
            Err(e) => {
                tracing::warn!(
                    planner_card_id = %planner_card_id,
                    error = %e,
                    "dispatcher push: active runtime lookup failed; skipping harness observation"
                );
                return None;
            }
        };
        if runtime.kind != WorkerSessionKind::SharedPlanner {
            return None;
        }
        let handle_state = runtime.handle_state_json.as_ref()?;
        if is_harness_snapshot_value(handle_state) {
            Some(runtime.id)
        } else {
            None
        }
    }
}

/// Resolve execution identity identically for live notifications and boot replay.
/// Execution IDs are opaque; a historical gate result keeps its author's key.
pub(crate) async fn resolve_harness_observation(
    repo: &dyn crate::db::RepoEventWrite,
    track_id: &TrackId,
    event: &Event,
) -> crate::error::Result<Option<HarnessObservation>> {
    if let Event::TaskFilePublicationSettled {
        task_id,
        operation_id,
    } = event
    {
        return crate::file_delivery::settlement::observation(
            repo,
            track_id,
            task_id,
            operation_id,
        )
        .await;
    }
    if let Event::TaskCandidateVerificationSettled {
        task_id,
        operation_id,
    } = event
    {
        return crate::file_delivery::verification_settlement::observation(
            repo,
            track_id,
            task_id,
            operation_id,
        )
        .await;
    }
    if matches!(event, Event::TaskGitDeliverySettled { .. }) {
        return git_delivery_settled::observation(repo, track_id, event).await;
    }
    if let Event::TaskExecutionSettled {
        task_id,
        operation_id,
    } = event
    {
        if !crate::isolated_codex::settled::relevant(repo, track_id, task_id, operation_id).await? {
            return Ok(None);
        }
        if let Some(observation) = crate::isolated_codex::settled::review_observation(
            repo,
            track_id,
            task_id,
            operation_id,
        )
        .await?
        {
            return Ok(Some(observation));
        }
    }
    let task_key = if let Event::TaskGateResult {
        task_id,
        idempotency_key,
        attempt,
        ..
    } = event
    {
        if task_id != idempotency_key {
            return Err(crate::error::CalmError::Conflict(
                "gate observation execution identity mismatch".into(),
            ));
        }
        // Validate the canonical reader address before either live push or boot replay renders it;
        // never use the current-key alias.
        calm_truth::track_fs_view::task_gate_log_path(task_id, *attempt).map_err(|error| {
            crate::error::CalmError::Conflict(format!("gate observation: {error:?}"))
        })?;
        let task = calm_truth::db::RepoRead::task_get(repo, task_id)
            .await?
            .ok_or_else(|| {
                crate::error::CalmError::Conflict(format!(
                    "gate observation: missing execution {task_id}"
                ))
            })?;
        if task.track_id != track_id.as_str() {
            return Err(crate::error::CalmError::Forbidden(
                "gate observation belongs to another track".into(),
            ));
        }
        Some(task.key)
    } else {
        None
    };
    let mut observation = harness_observation_from_event(track_id, event, task_key.as_deref());
    if let Some(observation) = observation.as_mut() {
        attach_report_block_refs(repo, event, observation).await;
    }
    Ok(observation)
}

/// Attach block ids / revs and `docRev` read from the report card now. Best-effort: refs are
/// attached only when the report as read still projects to the event's `body_after`; otherwise
/// both fields stay `None`.
async fn attach_report_block_refs(
    repo: &dyn crate::db::RepoRead,
    event: &Event,
    observation: &mut HarnessObservation,
) {
    let (
        Event::TrackReportEdited {
            card_id,
            body_after,
            ..
        },
        HarnessObservation::ReportEdited {
            track_id,
            doc_rev_after,
            blocks_after,
            ..
        },
    ) = (event, observation)
    else {
        return;
    };
    let snapshot =
        match crate::track_report_read::load_report_doc_snapshot(repo, card_id.as_str()).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(
                    %track_id,
                    report_card_id = %card_id,
                    %error,
                    "report edit observation: report read failed; the diff carries no block ids"
                );
                return;
            }
        };
    match calm_types::report_edit_diff::align_block_refs(body_after, &snapshot.blocks) {
        Some(refs) => {
            *doc_rev_after = Some(snapshot.doc_rev);
            *blocks_after = Some(refs);
        }
        None => tracing::debug!(
            %track_id,
            report_card_id = %card_id,
            doc_rev = snapshot.doc_rev,
            "report edit observation: report no longer projects to the event body; the diff carries no block ids"
        ),
    }
}

pub(crate) fn harness_observation_from_event(
    track_id: &TrackId,
    event: &Event,
    task_key: Option<&str>,
) -> Option<HarnessObservation> {
    match event {
        Event::TaskCandidateVerificationSettled { .. }
        | Event::TaskGitDeliverySettled { .. }
        | Event::TaskFilePublicationSettled { .. } => None, // requires the retained Operation / tasks row read above
        Event::TaskCompleted {
            idempotency_key,
            result,
            ..
        } => Some(HarnessObservation::TaskCompleted {
            idempotency_key: idempotency_key.clone(),
            result: result.clone(),
        }),
        Event::TaskFailed {
            idempotency_key,
            reason,
            ..
        } => Some(HarnessObservation::TaskFailed {
            idempotency_key: idempotency_key.clone(),
            error: reason.clone(),
        }),
        Event::TaskExecutionSettled { task_id, .. } => Some(HarnessObservation::SystemContext {
            text: format!(
                "Failed task execution {task_id} has stopped and its Operation has settled. Re-read calm.plan.list for the current attempt and recovery capability. Choose a same-contract recovery only when authorized; if User authorization is required, explain that next step. Isolated recovery uses a new workspace. Declared JSON consumers retain their original immutable input binding; other retained files remain evidence."
            ),
        }),
        // Gate log paths use the author key resolved from the execution row.
        Event::TaskGateResult {
            idempotency_key,
            passed,
            failing_step,
            exit_code,
            log_tail,
            attempt,
            status_detail,
            target,
            ..
        } => Some(HarnessObservation::TaskGateResult {
            idempotency_key: idempotency_key.clone(),
            key: task_key?.to_string(),
            passed: *passed,
            failing_step: failing_step.clone(),
            exit_code: *exit_code,
            log_tail: log_tail.clone(),
            attempt: *attempt,
            status_detail: status_detail.clone(),
            target: target.clone().map(Box::new),
        }),
        Event::TrackReportEdited {
            body_before,
            body_after,
            author,
            ..
        } => Some(HarnessObservation::ReportEdited {
            track_id: track_id.clone(),
            body_sha256: sha256_hex(body_after),
            body: body_after.clone(),
            author: Some(*author),
            body_before: Some(body_before.clone()),
            // Filled by `attach_report_block_refs`; this sync mapping cannot read the report.
            doc_rev_after: None,
            blocks_after: None,
        }),
        Event::WorkspaceLeased {
            card_id,
            lease_id,
            path,
            ..
        } => Some(HarnessObservation::WorkspaceLeased {
            track_id: track_id.clone(),
            card_id: card_id.clone(),
            lease_id: lease_id.clone(),
            path: path.clone(),
        }),
        Event::WorkspaceReleased {
            card_id, lease_id, ..
        } => Some(HarnessObservation::WorkspaceReleased {
            track_id: track_id.clone(),
            card_id: card_id.clone(),
            lease_id: lease_id.clone(),
        }),
        Event::ForgePrMerged { subject, .. } => Some(HarnessObservation::ForgePrMerged {
            track_id: track_id.clone(),
            pr_number: subject.pr_number,
        }),
        Event::ReviewRound {
            subject,
            head_sha,
            n,
            cap,
            converged,
            ..
        } => Some(HarnessObservation::ReviewRound {
            track_id: track_id.clone(),
            phase: subject.phase.clone(),
            slice_id: subject.slice_id.clone(),
            pr_number: subject.pr_number,
            head_sha: head_sha.clone(),
            n: *n,
            cap: *cap,
            converged: *converged,
        }),
        Event::RatifyRequested { reason, .. } => Some(HarnessObservation::RatifyRequested {
            track_id: track_id.clone(),
            reason: reason.clone(),
        }),
        Event::RatifyResolved { decision, .. } => Some(HarnessObservation::RatifyResolved {
            track_id: track_id.clone(),
            decision: *decision,
        }),
        Event::ForgeScanCompleted {
            overlapping_prs, ..
        } => Some(HarnessObservation::ForgeScanCompleted {
            track_id: track_id.clone(),
            overlapping_prs: overlapping_prs.clone(),
        }),
        Event::ForgePrOpened { pr_number, .. } => Some(HarnessObservation::ForgePrOpened {
            track_id: track_id.clone(),
            pr_number: *pr_number,
        }),
        Event::ForgePrChecks {
            pr_number,
            conclusion,
            ..
        } => Some(HarnessObservation::ForgePrChecks {
            track_id: track_id.clone(),
            pr_number: *pr_number,
            conclusion: conclusion.clone(),
        }),
        Event::ForgeIssueClosed { issue_number, .. } => {
            Some(HarnessObservation::ForgeIssueClosed {
                track_id: track_id.clone(),
                issue_number: *issue_number,
            })
        }
        Event::WorktreeProvisioned { card_id, path, .. } => {
            Some(HarnessObservation::WorktreeProvisioned {
                track_id: track_id.clone(),
                card_id: card_id.clone(),
                path: path.clone(),
            })
        }
        Event::WorktreeCommitted {
            card_id,
            commit_sha,
            branch,
            ..
        } => Some(HarnessObservation::WorktreeCommitted {
            track_id: track_id.clone(),
            card_id: card_id.clone(),
            commit_sha: commit_sha.clone(),
            branch: branch.clone(),
        }),
        Event::CodexHook {
            card_id,
            kind,
            hook_idempotency_key,
            ..
        } if kind == "hook.codex.stop" => Some(HarnessObservation::WorkerHookStop {
            track_id: track_id.clone(),
            card_id: card_id.clone(),
            kind: HarnessHookKind::CodexStop,
            idempotency_key: hook_idempotency_key.clone(),
        }),
        Event::ClaudeHook {
            card_id,
            kind,
            hook_idempotency_key,
            ..
        } if kind == "hook.claude.stop" => Some(HarnessObservation::WorkerHookStop {
            track_id: track_id.clone(),
            card_id: card_id.clone(),
            kind: HarnessHookKind::ClaudeStop,
            idempotency_key: hook_idempotency_key.clone(),
        }),
        Event::CodexHook { .. } | Event::ClaudeHook { .. } => None,
        Event::AreaUpdated(_)
        | Event::AreaDeleted { .. }
        | Event::TrackUpdated(_)
        | Event::TrackDeleted { .. }
        | Event::TrackLifecycleChanged { .. }
        | Event::CardAdded(_)
        | Event::CardUpdated(_)
        | Event::CardDeleted { .. }
        | Event::WorkerSessionStarted { .. }
        | Event::WorkerSessionStatusChanged { .. }
        | Event::WorkerSessionSuperseded { .. }
        | Event::HarnessItemAdded { .. }
        | Event::HarnessPhaseChanged { .. }
        | Event::HarnessTranscriptCleared { .. }
        | Event::HarnessUserMessageEnqueued { .. }
        | Event::HarnessQueueChanged { .. }
        | Event::OverlaySet(_)
        | Event::OverlayDeleted { .. }
        | Event::TerminalDeleted { .. }
        | Event::PluginState { .. }
        | Event::PluginToolRegistered { .. }
        | Event::CodexWorkerRequested { .. }
        | Event::TerminalWorkerRequested { .. }
        | Event::PlanUpdated { .. }
        | Event::TaskDispatched { .. }
        | Event::TaskContextFrozen { .. }
        | Event::TaskContextAdvanced { .. }
        | Event::ForgePrDiffRead { .. }
        | Event::ForgeIssueRead { .. }
        | Event::ProposalSubmitted { .. }
        | Event::ProposalResolved { .. }
        | Event::WorktreeRemoved { .. } => None,
    }
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

mod git_delivery_settled;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod recovery_tests;
