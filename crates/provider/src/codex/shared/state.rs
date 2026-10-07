//! Shared daemon state and connection ownership contracts.
use crate::codex::CodexAppServer;
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{process::Child, task::JoinHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SharedDaemonState {
    Idle,
    Starting,
    Running,
    Restarting,
    Failed,
}

impl SharedDaemonState {
    pub fn as_db_str(self) -> &'static str {
        match self {
            SharedDaemonState::Idle => "idle",
            SharedDaemonState::Starting => "starting",
            SharedDaemonState::Running => "running",
            SharedDaemonState::Restarting => "restarting",
            SharedDaemonState::Failed => "failed",
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "starting" => SharedDaemonState::Starting,
            "running" => SharedDaemonState::Running,
            "restarting" => SharedDaemonState::Restarting,
            "failed" => SharedDaemonState::Failed,
            _ => SharedDaemonState::Idle,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedDaemonRuntime {
    pub pid: i32,
    pub pgid: i32,
    pub boot_id: String,
    pub process_start_time: u64,
    pub started_at: i64,
}

#[derive(Debug, Clone, Serialize)]
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

/// Slow-lane retry ceiling for the self-heal loop. A code invariant, not a knob.
pub const HEAL_SLOW_RETRY_CEILING: Duration = Duration::from_secs(300);

/// Classified failure lanes for the self-heal loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Fast lane — the existing `BackoffState` knobs. Child exited,
    /// handshake/cold-start deadline, socket errors.
    Transient,
    /// Slow lane — spawn exec error, settings/config read error, guard
    /// refusal that is safe to retry (floor `restart_max_delay_ms`, ceiling
    /// [`HEAL_SLOW_RETRY_CEILING`]).
    Persistent,
    /// Slow lane, reconciliation-only rounds: a possible surviving daemon we could not prove
    /// gone. The spawn path is unreachable until a round proves absence.
    Unreconciled,
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
pub enum SupervisorState {
    Idle,
    Starting {
        backoff_until: Option<Instant>,
        socket_path: PathBuf,
    },
    Running {
        child: Option<Child>,
        client: Arc<CodexAppServer>,
        runtime: SharedDaemonRuntime,
        watcher: SupervisorWatcher,
    },
    Restarting {
        prev_pid: Option<i32>,
        reason: String,
        attempts: u32,
    },
    Failed {
        last_error: String,
        /// Heal-lane classification for this failure.
        class: FailureClass,
        since: Instant,
    },
}

pub enum WatcherKind {
    SpawnedChild,
    TakenOverPid { pid: i32 },
}

pub struct SupervisorWatcher {
    pub kind: WatcherKind,
    pub handle: JoinHandle<()>,
}

pub struct SupervisorCore {
    pub state: SupervisorState,
    pub attempts: u32,
    /// Bumped (`wrapping_add`) each time a Running state is installed. Consumers compare by
    /// equality only, never ordering.
    pub generation: u64,
}

pub struct LaunchedSharedDaemon {
    pub child: Option<Child>,
    pub client: Arc<CodexAppServer>,
    pub runtime: SharedDaemonRuntime,
    pub watcher: SupervisorWatcher,
}

impl SupervisorState {
    /// Return last_error string when present (Restarting.reason, Failed.last_error).
    /// None for Idle/Starting/Running.
    pub fn last_error(&self) -> Option<&str> {
        match self {
            SupervisorState::Restarting { reason, .. } => Some(reason.as_str()),
            SupervisorState::Failed { last_error, .. } => Some(last_error.as_str()),
            _ => None,
        }
    }

    /// DB string mapping for persistence + status_snapshot.
    pub fn as_shared_daemon_state(&self) -> SharedDaemonState {
        match self {
            SupervisorState::Idle => SharedDaemonState::Idle,
            SupervisorState::Starting { .. } => SharedDaemonState::Starting,
            SupervisorState::Running { .. } => SharedDaemonState::Running,
            SupervisorState::Restarting { .. } => SharedDaemonState::Restarting,
            SupervisorState::Failed { .. } => SharedDaemonState::Failed,
        }
    }
}
