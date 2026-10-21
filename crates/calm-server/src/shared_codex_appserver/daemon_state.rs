//! Shared daemon state and connection ownership contracts.
use super::heal_retry::FailureClass;
use crate::codex_appserver::CodexAppServer;
use crate::db::SharedDaemonState;
use std::sync::Arc;
use tokio::{process::Child, task::JoinHandle};

#[derive(Debug, Clone)]
pub struct SharedDaemonRuntime {
    pub pid: i32,
    pub pgid: i32,
    pub boot_id: String,
    pub process_start_time: u64,
    pub started_at: i64,
}

#[derive(Debug, Clone)]
pub struct SharedDaemonStatus {
    pub state: SharedDaemonState,
    pub sock: String,
    pub codex_home: String,
    pub runtime: Option<SharedDaemonRuntime>,
    pub cached_threads: usize,
    pub pending_count: usize,
    pub restart_count: u64,
    pub last_error: Option<String>,
}

/// Precondition validated under the core lock before any destructive work; the transition
/// serial is never released before the terminal Running/Failed write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacePrecondition {
    /// Settings respawn: replace whatever is installed.
    Always,
    /// Crash watcher: proceed only if the Running generation it observed
    /// dying is still the installed one (equality only, never ordering).
    GenerationIs(u64),
    /// Heal loop: proceed only when nothing is Running — the loop can never
    /// stomp a Running daemon a concurrent transition won.
    NotRunning,
}

/// Readiness snapshot published on every installed Running / terminal Failed and on every
/// transition entry, so `running: true` holds only while a Running incarnation is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonReadiness {
    pub generation: u64,
    pub running: bool,
}

/// Outcome of replacing the shared daemon incarnation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceOutcome {
    Replaced,
    /// The precondition failed: no reap, no spawn, nothing touched.
    PreconditionFailed,
}

/// Typestate companion to `SharedDaemonState`. `Child` MUST stay private; only transition
/// APIs may kill/replace. Sibling attribution (thread_cache, active_turns, pending) survives
/// process restarts and is NOT part of typestate.
// Keep the running process inline; smaller transition states need no allocation.
#[allow(clippy::large_enum_variant)]
pub(crate) enum SupervisorState {
    Idle,
    Starting,
    Running {
        child: Option<Child>,
        client: Arc<CodexAppServer>,
        runtime: SharedDaemonRuntime,
        watcher: SupervisorWatcher,
    },
    Restarting {
        reason: String,
    },
    Failed {
        last_error: String,
        /// Heal-lane classification for this failure.
        class: FailureClass,
    },
}

#[cfg(any(test, feature = "fixtures"))]
pub(crate) enum WatcherKind {
    SpawnedChild,
    TakenOverPid,
}

pub(crate) struct SupervisorWatcher {
    #[cfg(any(test, feature = "fixtures"))]
    pub kind: WatcherKind,
    pub handle: JoinHandle<()>,
}

pub(crate) struct SupervisorCore {
    pub state: SupervisorState,
    pub attempts: u32,
    /// Bumped (`wrapping_add`) each time a Running state is installed. Consumers compare by
    /// equality only, never ordering.
    pub generation: u64,
}

pub(crate) struct LaunchedSharedDaemon {
    pub child: Option<Child>,
    pub client: Arc<CodexAppServer>,
    pub runtime: SharedDaemonRuntime,
    pub watcher: SupervisorWatcher,
}

impl SupervisorState {
    /// Return last_error string when present (Restarting.reason, Failed.last_error).
    /// None for Idle/Starting/Running.
    pub(crate) fn last_error(&self) -> Option<&str> {
        match self {
            SupervisorState::Restarting { reason } => Some(reason.as_str()),
            SupervisorState::Failed { last_error, .. } => Some(last_error.as_str()),
            SupervisorState::Idle | SupervisorState::Starting | SupervisorState::Running { .. } => {
                None
            }
        }
    }

    /// DB string mapping for persistence + status_snapshot.
    pub(crate) fn as_shared_daemon_state(&self) -> SharedDaemonState {
        match self {
            SupervisorState::Idle => SharedDaemonState::Idle,
            SupervisorState::Starting => SharedDaemonState::Starting,
            SupervisorState::Running { .. } => SharedDaemonState::Running,
            SupervisorState::Restarting { .. } => SharedDaemonState::Restarting,
            SupervisorState::Failed { .. } => SharedDaemonState::Failed,
        }
    }
}
