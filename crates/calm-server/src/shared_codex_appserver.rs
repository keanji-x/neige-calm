//! Shared `codex app-server` supervisor: starts, supervises, and takes over a single
//! daemon for the whole server.

#[cfg(target_os = "macos")]
mod macos_process;
mod preserving_recovery;

use std::collections::HashSet;
use std::collections::VecDeque;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
#[cfg(feature = "fixtures")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, broadcast};
use tokio::task::JoinHandle;

use crate::codex_appserver::{
    ClientInfo, CodexAppServer, CodexConfig, CodexModel, InputItem, Notification,
    ThreadStartParams, redact_thread_start_config,
};
use crate::config::Config;
use crate::db::sqlite::session_projection_active_for_card_tx;
use crate::db::{Repo, SharedCodexDaemonUpdate, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::mcp_server::transport;
use crate::mcp_server::wiring::{card_mcp_thread_start_config, mint_and_persist_card_token};
use crate::model::{CardRole, now_ms};
use crate::pending_codex_threads::PendingThreadStartRegistry;
use crate::planner_model::TurnModelSelection;
use crate::proc_identity::{
    read_boot_id, read_proc_start_time, sigkill_verified_group_members, signal_process_group,
    verify_owned_pid,
};
use crate::routes::settings::load_settings;
use crate::session_projection_lookup::{
    merge_active_shared_thread_attribution, resolve_active_thread_for_card, resolve_card_for_thread,
};
use crate::session_projection_repo::AgentProvider;
use crate::shared_codex_home::{EXPECTED_MCP_SERVERS, SharedCodexHome};

pub type TurnId = String;

/// Ambient env keys forwarded verbatim into the spawned shared codex app-server; everything
/// else in the parent env is dropped by `env_clear()`, computed keys (including `PATH`) are set in
/// `apply_spawn_env`.
pub const SPAWN_ENV_PASSTHROUGH: &[&str] = &[
    // default-home fallback + `~` expansion in config paths; forwarded to MCP children
    "HOME",
    // codex's own child allow-lists forward these
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TZ",
    "TMPDIR",
    "TEMP",
    "TMP",
    // reqwest env-proxy autodetect honors these for API traffic
    "NO_PROXY",
    "no_proxy",
    "ALL_PROXY",
    "all_proxy",
    // TLS custom CA (SSL_CERT_DIR unused)
    "CODEX_CA_CERTIFICATE",
    "SSL_CERT_FILE",
    // diagnostics
    "RUST_LOG",
    "LOG_FORMAT",
    "RUST_BACKTRACE",
    // API-key-mode auth fallbacks; prod uses auth.json, kept so an API-key deployment
    // doesn't silently break. Still an allow-list.
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "OPENAI_ORGANIZATION",
    "OPENAI_PROJECT",
];

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

// A cold load must distinguish an explicitly suspended harness from legacy
// threads that have no runtime. Never load a failed harness without its token.
enum ColdResumeAuthorization {
    Skip,
    NoMcp,
    Token { role: CardRole, raw: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeMode {
    ColdRespawn,
    HotTakeover,
}

/// Slow-lane retry ceiling for the self-heal loop. A code invariant, not a knob.
pub const HEAL_SLOW_RETRY_CEILING: Duration = Duration::from_secs(300);

/// ±20% uniform jitter applied to every heal delay.
const HEAL_JITTER_FRACTION: f64 = 0.2;

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

/// Outcome of [`SharedCodexAppServer::transition_replace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceOutcome {
    Replaced,
    /// The precondition failed: no reap, no spawn, nothing touched.
    PreconditionFailed,
}

/// How the shared start body established the Running daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartedVia {
    Takeover,
    Spawn,
}

impl StartedVia {
    fn resume_mode(self) -> ResumeMode {
        match self {
            StartedVia::Takeover => ResumeMode::HotTakeover,
            StartedVia::Spawn => ResumeMode::ColdRespawn,
        }
    }
}

/// Result of the exhaustive per-shape resolution over a persisted daemon record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShapeResolution {
    /// Absence/death proven — identity columns may be NULLed (the durable
    /// proof-of-absence transition) and the spawn path reopened.
    ProvenAbsent,
    /// A possible survivor we could not prove gone. `needs_operator` marks
    /// the pid-NULL shape that names no process at all — the only shape that
    /// cannot self-resolve on process death.
    Unreconciled { needs_operator: bool },
}

/// The identity/context columns a Failed persist writes: NULL-only when absence is proven;
/// retained exactly as read while unreconciled.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FailedIdentity {
    pid: Option<i32>,
    pgid: Option<i32>,
    process_start_time: Option<u64>,
    boot_id: Option<String>,
    started_at: Option<i64>,
    sock_path: Option<String>,
    codex_home_path: Option<String>,
    daemon_env_signature: Option<String>,
}

impl FailedIdentity {
    /// Proven absence: identity NULL, sock/home point at this supervisor,
    /// no signature (there is no process for it to describe).
    fn proven_absent(sock: &Path, codex_home: &Path) -> Self {
        Self {
            pid: None,
            pgid: None,
            process_start_time: None,
            boot_id: None,
            started_at: None,
            sock_path: Some(sock.display().to_string()),
            codex_home_path: Some(codex_home.display().to_string()),
            daemon_env_signature: None,
        }
    }

    /// Unreconciled: retain the tuple exactly as read — identity presence IS the durable
    /// unreconciled marker.
    fn retained(record: &crate::db::SharedCodexDaemonRecord) -> Self {
        Self {
            pid: record.pid,
            pgid: record.pgid,
            process_start_time: record.process_start_time,
            boot_id: record.boot_id.clone(),
            started_at: record.started_at,
            sock_path: record.sock_path.clone(),
            codex_home_path: record.codex_home_path.clone(),
            daemon_env_signature: record.daemon_env_signature.clone(),
        }
    }
}

/// Last successfully-persisted Failed tuple; identical consecutive Failed writes are skipped.
/// Updated ONLY after the DB write returns Ok, so a failed write is retried, never masked.
#[derive(Default)]
struct FailedPersistDedup(std::sync::Mutex<Option<(String, FailureClass, FailedIdentity)>>);

impl FailedPersistDedup {
    fn should_skip(
        &self,
        last_error: &str,
        class: FailureClass,
        identity: &FailedIdentity,
    ) -> bool {
        self.0
            .lock()
            .expect("failed persist dedup mutex poisoned")
            .as_ref()
            .is_some_and(|(cached_error, cached_class, cached_identity)| {
                cached_error == last_error && *cached_class == class && cached_identity == identity
            })
    }

    /// Call ONLY after the DB write returned Ok.
    fn note_written(&self, last_error: String, class: FailureClass, identity: FailedIdentity) {
        *self.0.lock().expect("failed persist dedup mutex poisoned") =
            Some((last_error, class, identity));
    }

    fn clear(&self) {
        *self.0.lock().expect("failed persist dedup mutex poisoned") = None;
    }
}

/// RAII claim for the singleton heal task: `Drop` clears `heal_active`, so panic,
/// cancellation, abort, and normal exit all release the claim.
struct HealActiveGuard {
    flag: Arc<AtomicBool>,
}

impl Drop for HealActiveGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

/// Failure classification for the heal lanes: the Transient messages are exactly the
/// cold-start poll failures plus the crash-watcher exit shape; everything else is slow-lane.
fn classify_spawn_failure(err: &CalmError) -> FailureClass {
    let msg = err.to_string();
    if msg.contains("exited before initialize")
        || msg.contains("not initialized after")
        || msg.contains("app-server exited")
    {
        FailureClass::Transient
    } else {
        FailureClass::Persistent
    }
}

/// Marker for the ONE error whose transition outcome was never observed; callers must not
/// claim "terminalized + heal armed" for it.
const DETACHED_SPAWN_RESULT_LOST: &str =
    "detached spawn transition task ended without publishing a result";

fn detached_spawn_result_lost(err: &CalmError) -> bool {
    err.to_string().contains(DETACHED_SPAWN_RESULT_LOST)
}

/// ±20% uniform jitter; clock-derived entropy suffices because the heal loop only needs
/// desynchronization.
fn heal_jitter(delay: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    let unit = (nanos % 1_000_001) as f64 / 1_000_000.0;
    delay.mul_f64(1.0 - HEAL_JITTER_FRACTION + 2.0 * HEAL_JITTER_FRACTION * unit)
}

/// `/proc/<pid>/stat` state == `Z`. A zombie means "dead" only for post-reap verification
/// of a group WE just signaled; before any signal it proves nothing about the process group.
#[cfg(not(target_os = "macos"))]
fn proc_pid_is_zombie(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // The state field is the first token after the `(comm)` field.
    stat.rsplit(") ")
        .next()
        .and_then(|rest| rest.chars().next())
        .map(|state| state == 'Z')
        .unwrap_or(false)
}

/// macOS twin of the Linux `/proc/<pid>/stat` reader via `proc_pidinfo(PROC_PIDTBSDINFO)`;
/// `arg = 1` asks XNU to search the zombie list, without it an unreaped child answers `ESRCH`.
#[cfg(target_os = "macos")]
fn proc_pid_is_zombie(pid: i32) -> bool {
    use std::ffi::c_void;
    if pid <= 0 {
        return false;
    }
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is a zeroed `proc_bsdinfo` of exactly the size passed.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            1,
            &mut info as *mut libc::proc_bsdinfo as *mut c_void,
            size as libc::c_int,
        )
    };
    if written != size as libc::c_int {
        return false;
    }
    info.pbi_status == libc::SZOMB
}

#[cfg(all(test, target_os = "macos"))]
mod macos_zombie_tests {
    use super::proc_pid_is_zombie;
    use std::process::Command;

    #[test]
    fn exited_unreaped_child_is_a_zombie_until_waited() {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(proc_pid_is_zombie(pid));
        child.wait().unwrap();
        assert!(!proc_pid_is_zombie(pid));
    }

    #[test]
    fn live_child_is_not_a_zombie() {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        assert!(!proc_pid_is_zombie(pid));
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

/// `/proc/<pid>` presence — the ONLY probe allowed for the pid-partial shape: ownership is
/// unprovable (the pid may be recycled), so the record must never be reaped or signaled.
/// A zombie counts as present here.
fn proc_pid_present(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    Path::new(&format!("/proc/{pid}")).exists()
}

/// POST-group-reap "survivor still alive" criterion. Zombie-as-dead is valid here ONLY
/// because every caller has just signaled the verified group; pre-signal probes must not use it.
fn survivor_alive_after_group_reap(pid: i32, start_time: u64, boot_id: &str) -> bool {
    verify_owned_pid(pid, start_time, boot_id) && !proc_pid_is_zombie(pid)
}

/// Durable marker prefixes, human-readable; the machine rule is identity presence.
const UNRECONCILED_PREFIX: &str = "unreconciled: ";
const UNRECONCILED_NEEDS_OPERATOR_PREFIX: &str = "unreconciled-needs-operator: ";

fn unreconciled_prefix(needs_operator: bool) -> &'static str {
    if needs_operator {
        UNRECONCILED_NEEDS_OPERATOR_PREFIX
    } else {
        UNRECONCILED_PREFIX
    }
}

fn strip_unreconciled_prefix(last_error: &str) -> &str {
    last_error
        .strip_prefix(UNRECONCILED_NEEDS_OPERATOR_PREFIX)
        .or_else(|| last_error.strip_prefix(UNRECONCILED_PREFIX))
        .unwrap_or(last_error)
}

/// Read-side rule: `state='failed'` ∧ all identity columns NULL ⇒ safe to retry; any
/// identity column present ⇒ unreconciled, run the per-shape resolution before any spawn.
fn failed_row_identity_present(record: &crate::db::SharedCodexDaemonRecord) -> bool {
    SharedDaemonState::from_db_str(&record.state) == SharedDaemonState::Failed
        && (record.pid.is_some()
            || record.pgid.is_some()
            || record.process_start_time.is_some()
            || record.boot_id.is_some())
}

/// A pure function of the record's identity shape — NOT of its previous `last_error` — so
/// identical consecutive rounds dedup to a single DB write.
fn unreconciled_message(
    record: &crate::db::SharedCodexDaemonRecord,
    needs_operator: bool,
) -> String {
    if needs_operator {
        format!(
            "{UNRECONCILED_NEEDS_OPERATOR_PREFIX}persisted daemon record names no pid \
             (pgid={:?}, process_start_time={:?}, boot_id_present={}); \
             clear its identity columns to re-enable respawn",
            record.pgid,
            record.process_start_time,
            record.boot_id.is_some()
        )
    } else {
        format!(
            "{UNRECONCILED_PREFIX}possible surviving daemon (pid {:?}, pgid {:?}) \
             not proven gone; refusing to spawn until absence is proven",
            record.pid, record.pgid
        )
    }
}

#[derive(Clone)]
pub enum ThreadConfig {
    /// No per-card MCP credentials injected. Serializes to omitted `config`.
    NoMcp,
    /// Per-card MCP shell env injected through `shell_environment_policy.set`.
    McpShell {
        role: CardRole,
        socket_path: PathBuf,
        raw_token: String,
    },
}

impl ThreadConfig {
    /// An MCP thread's exec-shells get the kernel-led PATH through `shell_environment_policy.set`:
    /// codex re-applies `set` after restoring its login-shell snapshot, which can reset or reorder
    /// the daemon's inherited PATH (#1784).
    fn to_wire_config(&self) -> Result<Option<serde_json::Value>> {
        match self {
            Self::NoMcp => Ok(None),
            Self::McpShell {
                role,
                socket_path,
                raw_token,
            } => {
                let kernel_path = crate::kernel_bin_path::kernel_led_path()
                    .map_err(|error| CalmError::Internal(format!("thread PATH: {error}")))?;
                let path = kernel_path
                    .path_utf8()
                    .map_err(|error| CalmError::Internal(format!("thread PATH: {error}")))?;
                Ok(Some(card_mcp_thread_start_config(
                    socket_path,
                    raw_token,
                    *role,
                    path,
                )))
            }
        }
    }
}

#[derive(Clone)]
pub struct SharedThreadStartParams {
    pub cwd: String,
    pub approval_policy: String,
    pub sandbox_mode: String,
    pub developer_instructions: Option<String>,
    pub config: ThreadConfig,
}

impl std::fmt::Debug for SharedThreadStartParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_config = match self.config.to_wire_config() {
            Ok(config) => redact_thread_start_config(&config),
            Err(error) => serde_json::json!({ "unrenderable": error.to_string() }),
        };
        f.debug_struct("SharedThreadStartParams")
            .field("cwd", &self.cwd)
            .field("approval_policy", &self.approval_policy)
            .field("sandbox_mode", &self.sandbox_mode)
            .field("developer_instructions", &self.developer_instructions)
            .field("config", &redacted_config)
            .finish()
    }
}

#[derive(Debug)]
pub struct BackoffState {
    initial: Duration,
    max: Duration,
    stable_window: Duration,
    attempts: std::sync::atomic::AtomicU64,
    last_relaunch_at: std::sync::Mutex<Option<Instant>>,
}

impl BackoffState {
    pub fn new(initial: Duration, max: Duration) -> Self {
        let initial = initial.max(Duration::from_millis(1));
        let max = max.max(initial);
        Self {
            initial,
            max,
            stable_window: Duration::from_secs(60),
            attempts: std::sync::atomic::AtomicU64::new(0),
            last_relaunch_at: std::sync::Mutex::new(None),
        }
    }

    pub fn reset(&self) {
        self.attempts.store(0, Ordering::SeqCst);
        *self
            .last_relaunch_at
            .lock()
            .expect("backoff relaunch timestamp mutex poisoned") = None;
    }

    pub fn note_relaunch_now(&self) {
        *self
            .last_relaunch_at
            .lock()
            .expect("backoff relaunch timestamp mutex poisoned") = Some(Instant::now());
    }

    pub fn next_delay(&self) -> Duration {
        self.reset_if_stable();
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        bounded_exponential_backoff(self.initial, self.max, attempt)
    }

    /// Slow heal lane: same attempts counter as [`Self::next_delay`], floor = this state's
    /// max, caller-supplied ceiling.
    pub fn next_slow_delay(&self, ceiling: Duration) -> Duration {
        self.reset_if_stable();
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        bounded_exponential_backoff(self.max, ceiling.max(self.max), attempt)
    }

    fn reset_if_stable(&self) {
        let Some(last_relaunch_at) = *self
            .last_relaunch_at
            .lock()
            .expect("backoff relaunch timestamp mutex poisoned")
        else {
            return;
        };
        if last_relaunch_at.elapsed() >= self.stable_window {
            self.reset();
        }
    }

    #[cfg(any(test, feature = "fixtures"))]
    pub fn simulate_stable_run_for(&self, duration: Duration) {
        *self
            .last_relaunch_at
            .lock()
            .expect("backoff relaunch timestamp mutex poisoned") = Some(
            Instant::now()
                .checked_sub(duration)
                .unwrap_or_else(Instant::now),
        );
    }
}

pub fn bounded_exponential_backoff(initial: Duration, max: Duration, attempt: u64) -> Duration {
    let shift = attempt.min(31);
    let factor = 1_u32 << shift;
    initial.saturating_mul(factor).min(max)
}

/// Pagination guard for [`SharedCodexAppServer::model_list`]. Codex answers
/// the whole catalog in one page today; this only bounds a peer that never
/// clears `nextCursor`.
const MODEL_LIST_MAX_PAGES: usize = 20;

pub type NotificationFanout = broadcast::Sender<Notification>;

pub struct SharedCodexAppServer {
    recovery: Option<crate::semantic_recovery::RecoveryService>,
    sock: PathBuf,
    kernel_mcp_socket_path: PathBuf,
    home: Arc<SharedCodexHome>,
    repo: Arc<dyn Repo>,
    thread_cache: Arc<DashMap<String, String>>,
    active_turns: Arc<DashMap<String, String>>,
    /// Threads whose owning track/area is being deleted. A late turn/start
    /// response or notification is interrupted instead of becoming active
    /// after the owning workspace has been recycled.
    sealed_turn_threads: Arc<DashMap<String, ()>>,
    restart_backoff: BackoffState,
    /// Cold-start deadline for a freshly spawned child to bind its socket and answer
    /// `initialize`; codex may spend minutes backfilling its state db before the socket exists.
    start_timeout: Duration,
    /// Stop-grace ceiling for every daemon termination: SIGTERM → exit-driven wait → straggler
    /// cleanup. A cooperative daemon pays its actual exit time, never the full grace.
    stop_grace: Duration,
    notifications: NotificationFanout,
    pending_codex_threads_handle: Option<Arc<PendingThreadStartRegistry>>,
    kernel_initiated_threads: Arc<Mutex<HashSet<String>>>,
    /// Bounded tombstones for threads a committed delete forgot; see [`ForgottenThreads`].
    forgotten_threads: Arc<Mutex<ForgottenThreads>>,
    kernel_thread_start_serial: Arc<Mutex<()>>,
    /// Fences the resume replay against a committed delete's cache cleanup. Not
    /// `kernel_thread_start_serial`: the respawn's resume loop runs inside that one and tokio
    /// mutexes are not reentrant. Lock order: `kernel_thread_start_serial` → `resume_replay_serial`.
    resume_replay_serial: Arc<Mutex<()>>,
    codex_bin: String,
    log_dir: PathBuf,
    restart_count: std::sync::atomic::AtomicU64,
    /// Wall-clock ms of the most recent successful daemon (re)connect; `0` until the first connect.
    daemon_connected_at_ms: AtomicI64,
    needs_respawn_on_next_thread_start: Arc<AtomicBool>,
    /// Typestate-companion state machine.
    core: Arc<tokio::sync::Mutex<SupervisorCore>>,
    /// Serializes process transitions.
    transition_serial: Arc<tokio::sync::Mutex<()>>,
    /// Singleton claim for the background heal task; cleared by the RAII [`HealActiveGuard`].
    heal_active: Arc<AtomicBool>,
    /// Slow-lane immediate wake on external change; the heal loop sleeps in select(sleep, nudge).
    heal_nudge: Arc<tokio::sync::Notify>,
    /// Post-Ok dedup of Failed DB writes.
    failed_persist_dedup: FailedPersistDedup,
    /// Readiness channel: stamped on every installed Running and terminal Failed, and
    /// INVALIDATED on transition ENTRY, so a transitional daemon is never mistaken for Running.
    readiness: tokio::sync::watch::Sender<DaemonReadiness>,
    /// Fixtures-only interleaving gate: the heal loop's Ok path awaits this AFTER
    /// `ensure_running` succeeded but BEFORE it releases the singleton claim.
    #[cfg(feature = "fixtures")]
    heal_post_ok_gate: Arc<tokio::sync::Mutex<()>>,
    /// Fixtures-only interleaving gate: `transition_replace` awaits this right after the
    /// transition ENTRY and before the reap/start body.
    #[cfg(feature = "fixtures")]
    transition_entry_gate: Arc<tokio::sync::Mutex<()>>,
    /// Test seam: abort handle of the most recent detached spawn transition task.
    #[cfg(feature = "fixtures")]
    detached_spawn_task: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
    ingest_url: String,
    #[cfg(feature = "fixtures")]
    fake: Option<Arc<FakeSharedCodexAppServer>>,
}

/// Owns deletion-time thread seals; dropping an unfinished step rolls every seal back, and
/// `retain` transfers the quiesced ids to the transaction half of the deletion saga.
pub(crate) struct DeletionThreadSeals {
    daemon: Arc<SharedCodexAppServer>,
    thread_ids: Vec<String>,
    rollback_on_drop: bool,
}

impl DeletionThreadSeals {
    pub(crate) fn new(daemon: Arc<SharedCodexAppServer>) -> Self {
        Self {
            daemon,
            thread_ids: Vec::new(),
            rollback_on_drop: true,
        }
    }

    pub(crate) fn seal(&mut self, thread_id: impl Into<String>) {
        let thread_id = thread_id.into();
        if self
            .thread_ids
            .iter()
            .any(|existing| existing == &thread_id)
        {
            return;
        }
        self.daemon.seal_turn_thread_for_deletion(&thread_id);
        self.thread_ids.push(thread_id);
    }

    pub(crate) fn retain(mut self) -> Vec<String> {
        self.thread_ids.sort();
        self.thread_ids.dedup();
        self.rollback_on_drop = false;
        std::mem::take(&mut self.thread_ids)
    }
}

impl Drop for DeletionThreadSeals {
    fn drop(&mut self) {
        if self.rollback_on_drop {
            for thread_id in &self.thread_ids {
                self.daemon.unseal_turn_thread_after_rollback(thread_id);
            }
        }
    }
}

#[cfg(feature = "fixtures")]
pub type StartedThreadParam = (Option<String>, bool, Option<CardRole>);

#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct TurnStartReturnHook {
    pub entered: Arc<tokio::sync::Notify>,
    pub release: Arc<tokio::sync::Notify>,
}

#[cfg(feature = "fixtures")]
pub struct FakeSharedCodexAppServer {
    next_thread: AtomicU64,
    next_turn: AtomicU64,
    fail_next_thread_start: AtomicBool,
    fail_thread_resume: AtomicBool,
    resumed_threads: std::sync::Mutex<Vec<(String, bool)>>,
    /// Sticky, unlike `fail_next_thread_start`: the RETRY behaviour is what is under test.
    fail_turn_start: AtomicBool,
    /// Answer `turn/start` the way codex does when it sees the input and says no, as opposed
    /// to not answering at all; the two take opposite paths in `maybe_issue_turn`.
    reject_turn_start: AtomicBool,
    /// A scripted `config/read` answer; without it every read is an OUTAGE, and "could not be
    /// asked" vs "answered, naming no model" are two different paths.
    config_read: std::sync::Mutex<Option<CodexConfig>>,
    /// Answer `config/read` the way codex does when it sees the request and
    /// refuses it — an answer, not an outage. The two take opposite paths.
    reject_config_read: AtomicBool,
    /// Answer `model/list` the way codex does when it refuses the request.
    reject_model_list: AtomicBool,
    fail_turn_interrupt: AtomicBool,
    started_thread_params: std::sync::Mutex<Vec<StartedThreadParam>>,
    started_turns: std::sync::Mutex<Vec<(String, Vec<InputItem>)>>,
    /// The model selection each `turn/start` carried, in the same order as `started_turns`.
    started_turn_selections: std::sync::Mutex<Vec<(String, TurnModelSelection)>>,
    /// The `clientUserMessageId` each `turn/start` carried, in the same order as `started_turns`.
    started_turn_client_ids: std::sync::Mutex<Vec<Option<String>>>,
    interrupted_turns: std::sync::Mutex<Vec<(String, String)>>,
    turn_start_return_hook: std::sync::Mutex<Option<TurnStartReturnHook>>,
    /// Every `turn/steer` this fake was handed, in order.
    steered_turns: std::sync::Mutex<Vec<SteeredTurnParam>>,
    /// Answer the next `turn/steer` with codex's exact `-32600` refusal sentence, as opposed
    /// to not answering at all. `None` accepts.
    reject_turn_steer: std::sync::Mutex<Option<String>>,
    /// Answer the next `turn/steer` the way the client does when codex does NOT answer (a
    /// timeout); the request is still recorded, because on the wire it did go out.
    fail_turn_steer: AtomicBool,
    /// Same shape as `turn_start_return_hook`: hold `turn/steer` inside the
    /// daemon, after it has recorded the request, until the test releases it.
    turn_steer_return_hook: std::sync::Mutex<Option<TurnStartReturnHook>>,
}

/// One recorded `turn/steer`: thread, `expectedTurnId`, input, `clientUserMessageId`.
#[cfg(feature = "fixtures")]
pub type SteeredTurnParam = (String, String, Vec<InputItem>, Option<String>);

#[cfg(feature = "fixtures")]
impl FakeSharedCodexAppServer {
    fn new() -> Self {
        Self {
            next_thread: AtomicU64::new(1),
            next_turn: AtomicU64::new(1),
            fail_next_thread_start: AtomicBool::new(false),
            fail_thread_resume: AtomicBool::new(false),
            resumed_threads: std::sync::Mutex::new(Vec::new()),
            fail_turn_start: AtomicBool::new(false),
            reject_turn_start: AtomicBool::new(false),
            config_read: std::sync::Mutex::new(None),
            reject_config_read: AtomicBool::new(false),
            reject_model_list: AtomicBool::new(false),
            fail_turn_interrupt: AtomicBool::new(false),
            started_thread_params: std::sync::Mutex::new(Vec::new()),
            started_turns: std::sync::Mutex::new(Vec::new()),
            started_turn_selections: std::sync::Mutex::new(Vec::new()),
            started_turn_client_ids: std::sync::Mutex::new(Vec::new()),
            interrupted_turns: std::sync::Mutex::new(Vec::new()),
            turn_start_return_hook: std::sync::Mutex::new(None),
            steered_turns: std::sync::Mutex::new(Vec::new()),
            reject_turn_steer: std::sync::Mutex::new(None),
            fail_turn_steer: AtomicBool::new(false),
            turn_steer_return_hook: std::sync::Mutex::new(None),
        }
    }
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

pub(crate) struct RunningProcessParts {
    child: Option<Child>,
    runtime: SharedDaemonRuntime,
    watcher: SupervisorWatcher,
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

impl SharedCodexAppServer {
    pub fn new_stub(repo: Arc<dyn Repo>) -> Arc<Self> {
        Self::new_stub_inner(repo, None, false)
    }

    #[cfg(feature = "fixtures")]
    pub fn new_stub_with_pending(
        repo: Arc<dyn Repo>,
        pending_codex_threads_handle: Option<Arc<PendingThreadStartRegistry>>,
    ) -> Arc<Self> {
        Self::new_stub_inner(repo, pending_codex_threads_handle, false)
    }

    #[cfg(feature = "fixtures")]
    pub fn new_fake_running_with_pending(
        repo: Arc<dyn Repo>,
        pending_codex_threads_handle: Option<Arc<PendingThreadStartRegistry>>,
    ) -> Arc<Self> {
        Self::new_stub_inner(repo, pending_codex_threads_handle, true)
    }

    fn new_stub_inner(
        repo: Arc<dyn Repo>,
        pending_codex_threads_handle: Option<Arc<PendingThreadStartRegistry>>,
        _fake_running: bool,
    ) -> Arc<Self> {
        let root = std::env::temp_dir().join(format!(
            "neige-shared-codex-appserver-stub-{}",
            uuid::Uuid::new_v4()
        ));
        let legacy = root.join("codex-homes");
        let home = Arc::new(SharedCodexHome::new(root.join("codex-home"), legacy));
        let (tx, _) = broadcast::channel(16);
        Arc::new(Self {
            recovery: None,
            sock: root.join("run/codex-appserver.sock"),
            kernel_mcp_socket_path: transport::default_socket_path(&root),
            home,
            repo,
            thread_cache: Arc::new(DashMap::new()),
            active_turns: Arc::new(DashMap::new()),
            sealed_turn_threads: Arc::new(DashMap::new()),
            restart_backoff: BackoffState::new(Duration::from_millis(250), Duration::from_secs(10)),
            start_timeout: Duration::from_secs(120),
            stop_grace: Duration::from_secs(60),
            notifications: tx,
            pending_codex_threads_handle,
            kernel_initiated_threads: Arc::new(Mutex::new(HashSet::new())),
            forgotten_threads: Arc::new(Mutex::new(ForgottenThreads::default())),
            kernel_thread_start_serial: Arc::new(Mutex::new(())),
            resume_replay_serial: Arc::new(Mutex::new(())),
            codex_bin: "codex".into(),
            log_dir: root.join("logs/shared-codex-appserver"),
            restart_count: std::sync::atomic::AtomicU64::new(0),
            daemon_connected_at_ms: AtomicI64::new(0),
            needs_respawn_on_next_thread_start: Arc::new(AtomicBool::new(false)),
            core: Arc::new(tokio::sync::Mutex::new(SupervisorCore {
                state: SupervisorState::Idle,
                attempts: 0,
                generation: 0,
            })),
            transition_serial: Arc::new(tokio::sync::Mutex::new(())),
            heal_active: Arc::new(AtomicBool::new(false)),
            heal_nudge: Arc::new(tokio::sync::Notify::new()),
            failed_persist_dedup: FailedPersistDedup::default(),
            readiness: tokio::sync::watch::Sender::new(DaemonReadiness {
                generation: 0,
                running: false,
            }),
            #[cfg(feature = "fixtures")]
            heal_post_ok_gate: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(feature = "fixtures")]
            transition_entry_gate: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(feature = "fixtures")]
            detached_spawn_task: std::sync::Mutex::new(None),
            ingest_url: "http://127.0.0.1:0".into(),
            #[cfg(feature = "fixtures")]
            fake: _fake_running.then(|| Arc::new(FakeSharedCodexAppServer::new())),
        })
    }

    pub fn new(cfg: &Config, home: Arc<SharedCodexHome>, repo: Arc<dyn Repo>) -> Arc<Self> {
        Self::new_with_pending(cfg, home, repo, None)
    }

    pub fn new_with_pending(
        cfg: &Config,
        home: Arc<SharedCodexHome>,
        repo: Arc<dyn Repo>,
        pending_codex_threads_handle: Option<Arc<PendingThreadStartRegistry>>,
    ) -> Arc<Self> {
        Self::new_with_recovery(cfg, home, repo, pending_codex_threads_handle, None)
    }

    pub(crate) fn new_with_recovery(
        cfg: &Config,
        home: Arc<SharedCodexHome>,
        repo: Arc<dyn Repo>,
        pending_codex_threads_handle: Option<Arc<PendingThreadStartRegistry>>,
        recovery: Option<crate::semantic_recovery::RecoveryService>,
    ) -> Arc<Self> {
        let data_dir = cfg.data_dir_resolved();
        let (tx, _) = broadcast::channel(1024);
        Arc::new(Self {
            recovery,
            sock: data_dir.join("run/codex-appserver.sock"),
            kernel_mcp_socket_path: transport::default_socket_path(&data_dir),
            home,
            repo,
            thread_cache: Arc::new(DashMap::new()),
            active_turns: Arc::new(DashMap::new()),
            sealed_turn_threads: Arc::new(DashMap::new()),
            restart_backoff: BackoffState::new(
                Duration::from_millis(cfg.shared_codex_appserver_restart_initial_delay_ms),
                Duration::from_millis(cfg.shared_codex_appserver_restart_max_delay_ms),
            ),
            start_timeout: Duration::from_secs(cfg.shared_codex_appserver_start_timeout_secs),
            stop_grace: Duration::from_secs(cfg.shared_codex_appserver_stop_grace_secs),
            notifications: tx,
            pending_codex_threads_handle,
            kernel_initiated_threads: Arc::new(Mutex::new(HashSet::new())),
            forgotten_threads: Arc::new(Mutex::new(ForgottenThreads::default())),
            kernel_thread_start_serial: Arc::new(Mutex::new(())),
            resume_replay_serial: Arc::new(Mutex::new(())),
            codex_bin: cfg.codex_bin.clone(),
            log_dir: cfg.shared_codex_appserver_log_dir_resolved(),
            restart_count: std::sync::atomic::AtomicU64::new(0),
            daemon_connected_at_ms: AtomicI64::new(0),
            needs_respawn_on_next_thread_start: Arc::new(AtomicBool::new(false)),
            core: Arc::new(tokio::sync::Mutex::new(SupervisorCore {
                state: SupervisorState::Idle,
                attempts: 0,
                generation: 0,
            })),
            transition_serial: Arc::new(tokio::sync::Mutex::new(())),
            heal_active: Arc::new(AtomicBool::new(false)),
            heal_nudge: Arc::new(tokio::sync::Notify::new()),
            failed_persist_dedup: FailedPersistDedup::default(),
            readiness: tokio::sync::watch::Sender::new(DaemonReadiness {
                generation: 0,
                running: false,
            }),
            #[cfg(feature = "fixtures")]
            heal_post_ok_gate: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(feature = "fixtures")]
            transition_entry_gate: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(feature = "fixtures")]
            detached_spawn_task: std::sync::Mutex::new(None),
            ingest_url: cfg.codex_ingest_url_resolved(),
            #[cfg(feature = "fixtures")]
            fake: None,
        })
    }

    /// The shared CODEX_HOME handle. `GET /api/models` reads its
    /// `config.toml` directly when no daemon connection exists.
    pub fn shared_home(&self) -> &SharedCodexHome {
        &self.home
    }

    pub fn codex_home_path(&self) -> &std::path::Path {
        self.home.path()
    }

    pub async fn start_or_takeover(self: &Arc<Self>) -> Result<()> {
        // The whole boot sequence is ONE serialized transition: the owned serial is threaded down
        // the `_locked` chain by value and released only after the terminal Running/Failed write.
        // Thread resume runs after the serial is released (not a transition).
        let serial = Arc::clone(&self.transition_serial).lock_owned().await;
        self.rebuild_thread_cache_from_db().await?;
        let via = self.start_body_locked(serial, false, None).await?;
        self.resume_cached_threads(via.resume_mode()).await;
        Ok(())
    }

    /// (Re)establish a running daemon from any non-Running state; the `NotRunning` precondition
    /// is re-validated under the transition serial so the heal loop can never stomp a Running daemon.
    pub async fn ensure_running(self: &Arc<Self>) -> Result<()> {
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            return Ok(());
        }
        if matches!(
            self.core.lock().await.state,
            SupervisorState::Running { .. }
        ) {
            return Ok(());
        }
        // Both outcomes are success: Replaced means we established Running;
        // PreconditionFailed means a concurrent transition already did.
        self.transition_replace(
            "self-heal: daemon not running",
            ReplacePrecondition::NotRunning,
            false,
            None,
        )
        .await?;
        Ok(())
    }

    /// Subscribe to the readiness channel.
    pub fn readiness_receiver(&self) -> tokio::sync::watch::Receiver<DaemonReadiness> {
        self.readiness.subscribe()
    }

    /// Fixtures-only: publish a readiness value directly.
    #[cfg(feature = "fixtures")]
    pub fn publish_readiness_for_test(&self, generation: u64, running: bool) {
        self.readiness.send_replace(DaemonReadiness {
            generation,
            running,
        });
    }

    /// Preflight message carrying the live failure; preflights stay non-blocking.
    pub fn not_running_message(&self) -> String {
        let last_error = self
            .core
            .try_lock()
            .ok()
            .and_then(|core| core.state.last_error().map(String::from));
        match last_error {
            Some(e) => format!(
                "shared codex app-server is not running (last error: {e}); \
                 supervisor is retrying in the background — retry shortly"
            ),
            None => "shared codex app-server is not running; \
                     supervisor is retrying in the background — retry shortly"
                .to_string(),
        }
    }

    pub fn not_running_error(&self) -> CalmError {
        CalmError::Internal(self.not_running_message())
    }

    pub async fn thread_start_for_card(
        self: &Arc<Self>,
        card_id: &str,
        _role: CardRole,
        _track_id: Option<&str>,
        params: SharedThreadStartParams,
    ) -> Result<String> {
        let _start_guard = self.kernel_thread_start_serial.lock().await;
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            fake.started_thread_params
                .lock()
                .expect("fake shared codex thread params mutex poisoned")
                .push((
                    params.developer_instructions.clone(),
                    matches!(params.config, ThreadConfig::NoMcp),
                    Some(_role),
                ));
        }
        let thread_id = self.thread_start_mint_inner(card_id, params).await?;
        tracing::info!(
            target = "shared_codex_daemon::thread_start",
            %card_id,
            thread_id = %thread_id,
            "shared codex app-server thread started"
        );
        Ok(thread_id)
    }

    /// Kernel-only thread mint: performs `thread/start` and populates in-memory caches without
    /// touching durable runtime rows.
    pub async fn thread_start_mint_for_card(
        self: &Arc<Self>,
        card_id: &str,
        params: SharedThreadStartParams,
    ) -> Result<String> {
        let _start_guard = self.kernel_thread_start_serial.lock().await;
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            fake.started_thread_params
                .lock()
                .expect("fake shared codex thread params mutex poisoned")
                .push((
                    params.developer_instructions.clone(),
                    matches!(params.config, ThreadConfig::NoMcp),
                    None,
                ));
        }
        self.thread_start_mint_inner(card_id, params).await
    }

    /// Worker mint: inject per-card MCP shell credentials without Planner tool delegation.
    pub async fn thread_start_mint_mcp_shell(
        self: &Arc<Self>,
        card_id: &str,
        cwd: String,
        developer_instructions: Option<String>,
        socket_path: PathBuf,
        raw_token: String,
    ) -> Result<String> {
        let params = SharedThreadStartParams {
            cwd,
            approval_policy: "never".into(),
            sandbox_mode: "workspace-write".into(),
            developer_instructions,
            config: ThreadConfig::McpShell {
                role: CardRole::Worker,
                socket_path,
                raw_token,
            },
        };
        let _start_guard = self.kernel_thread_start_serial.lock().await;
        self.thread_start_mint_inner(card_id, params).await
    }

    /// Caller MUST hold `kernel_thread_start_serial`.
    async fn thread_start_mint_inner(
        self: &Arc<Self>,
        card_id: &str,
        params: SharedThreadStartParams,
    ) -> Result<String> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            if fake.fail_next_thread_start.swap(false, Ordering::SeqCst) {
                return Err(CalmError::CodexAppServer(
                    "forced thread/start failure".into(),
                ));
            }
            let n = fake.next_thread.fetch_add(1, Ordering::SeqCst);
            let thread_id = format!("fake-thread-{n:04}");
            self.kernel_initiated_threads
                .lock()
                .await
                .insert(thread_id.clone());
            self.thread_cache
                .insert(thread_id.clone(), card_id.to_string());
            return Ok(thread_id);
        }
        self.reap_and_respawn_with_current_settings().await?;
        let client = self.connected_client().await?;
        let config = params.config.to_wire_config()?;
        let semantic_recovery = self.recovery.is_some()
            && self.repo.card_role_get(card_id).await? == Some(CardRole::Planner);
        let tools = if semantic_recovery {
            vec![crate::semantic_recovery::descriptor()]
        } else {
            Vec::new()
        };
        let thread = client
            .thread_start_with_params_and_tools(
                ThreadStartParams {
                    cwd: params.cwd,
                    approval_policy: params.approval_policy,
                    sandbox_mode: params.sandbox_mode,
                    developer_instructions: params.developer_instructions,
                    config,
                },
                tools,
            )
            .await?;
        let thread_id = thread
            .thread_id()
            .ok_or_else(|| CalmError::CodexAppServer("thread/start returned no thread.id".into()))?
            .to_string();
        if semantic_recovery {
            crate::semantic_recovery::register(self.repo.as_ref(), card_id, &thread_id).await?;
        }
        self.kernel_initiated_threads
            .lock()
            .await
            .insert(thread_id.clone());
        self.thread_cache
            .insert(thread_id.clone(), card_id.to_string());
        Ok(thread_id)
    }

    /// If runtime settings changed, synchronously respawn the daemon so
    /// later TUI-started `thread/start` calls hit a process with current env.
    pub async fn ensure_respawn_for_current_settings(self: &Arc<Self>) -> Result<()> {
        let _start_guard = self.kernel_thread_start_serial.lock().await;
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            self.needs_respawn_on_next_thread_start
                .store(false, Ordering::Release);
            return Ok(());
        }
        self.reap_and_respawn_with_current_settings().await
    }

    /// Planner-harness reconciliation turn issuance goes through `IssueTurnHandle`; direct
    /// callers here are non-harness boot/operation paths or tests. `client_user_message_id`
    /// comes back as `item.clientId` on the echoed `userMessage`.
    pub async fn turn_start(
        &self,
        thread_id: &str,
        items: Vec<InputItem>,
        selection: &TurnModelSelection,
        client_user_message_id: Option<&str>,
    ) -> Result<TurnId> {
        if self.sealed_turn_threads.contains_key(thread_id) {
            return Err(CalmError::Conflict(format!(
                "thread {thread_id} is sealed because its track is being deleted"
            )));
        }
        if !self.thread_cache.contains_key(thread_id) {
            tracing::warn!(
                target = "shared_codex_daemon::mapping_miss",
                %thread_id,
                method = "turn/start",
                "turn/start for thread missing shared daemon card mapping"
            );
        }
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            if fake.reject_turn_start.load(Ordering::SeqCst) {
                return Err(CalmError::CodexRefused(
                    "turn/start failed: unknown model (code -32602)".into(),
                ));
            }
            if fake.fail_turn_start.load(Ordering::SeqCst) {
                return Err(CalmError::CodexAppServer(
                    "forced turn/start failure for test".into(),
                ));
            }
            let n = fake.next_turn.fetch_add(1, Ordering::SeqCst);
            let turn_id = format!("fake-turn-{n:04}");
            fake.started_turns
                .lock()
                .expect("fake shared codex started turns mutex poisoned")
                .push((thread_id.to_string(), items.clone()));
            fake.started_turn_selections
                .lock()
                .expect("fake shared codex turn selections mutex poisoned")
                .push((thread_id.to_string(), selection.clone()));
            fake.started_turn_client_ids
                .lock()
                .expect("fake shared codex turn client ids mutex poisoned")
                .push(client_user_message_id.map(ToOwned::to_owned));
            let hook = fake
                .turn_start_return_hook
                .lock()
                .expect("fake shared codex turn-start hook mutex poisoned")
                .take();
            if let Some(hook) = hook {
                hook.entered.notify_one();
                hook.release.notified().await;
            }
            self.active_turns
                .insert(thread_id.to_string(), turn_id.clone());
            let _ = self.notifications.send(Notification::TurnStarted {
                thread_id: thread_id.to_string(),
                turn: serde_json::json!({ "id": turn_id, "input_len": items.len() }),
            });
            if self.sealed_turn_threads.contains_key(thread_id) {
                self.turn_interrupt(thread_id, &turn_id).await?;
                self.active_turns
                    .remove_if(thread_id, |_, active| active == &turn_id);
                return Err(CalmError::Conflict(format!(
                    "thread {thread_id} was sealed while turn/start was in flight"
                )));
            }
            return Ok(turn_id);
        }
        let client = self.connected_client().await?;
        let turn = client
            .turn_start_with_client_id(thread_id, items, selection, client_user_message_id)
            .await?;
        let turn_id = turn
            .turn_id()
            .map(ToOwned::to_owned)
            .ok_or_else(|| CalmError::CodexAppServer("turn/start returned no turn.id".into()))?;
        self.active_turns
            .insert(thread_id.to_string(), turn_id.clone());
        if self.sealed_turn_threads.contains_key(thread_id) {
            self.turn_interrupt(thread_id, &turn_id).await?;
            self.active_turns
                .remove_if(thread_id, |_, active| active == &turn_id);
            return Err(CalmError::Conflict(format!(
                "thread {thread_id} was sealed while turn/start was in flight"
            )));
        }
        Ok(turn_id)
    }

    /// Whether a live app-server connection exists right now. Awaits the core lock rather than
    /// `try_lock`-ing it, and does not report the fixtures fake as connected.
    pub async fn has_connection(&self) -> bool {
        self.running_client().await.is_some()
    }

    /// `model/list`, drained across codex's pagination cursor. Read-only and connection-only:
    /// it never spawns or heals the daemon.
    pub async fn model_list(&self, deadline: tokio::time::Instant) -> Result<Vec<CodexModel>> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref()
            && fake.reject_model_list.load(Ordering::SeqCst)
        {
            return Err(CalmError::CodexRefused(
                "model/list failed: catalog unavailable for this account (code -32603)".into(),
            ));
        }
        let client = self.connected_client().await?;
        let mut models: Vec<CodexModel> = Vec::new();
        let mut skipped = 0usize;
        let mut cursor: Option<String> = None;
        // A server that echoes a cursor forever would otherwise pin this loop.
        for _ in 0..MODEL_LIST_MAX_PAGES {
            let page = client.model_list(cursor.as_deref(), deadline).await?;
            // Entries are decoded ONE AT A TIME; a malformed one is skipped rather than failing the
            // page, because an all-or-nothing decode would present a fabricated outage.
            for entry in page.data {
                match serde_json::from_value::<CodexModel>(entry) {
                    Ok(model) => models.push(model),
                    Err(e) => {
                        skipped += 1;
                        tracing::warn!(
                            target = "shared_codex_daemon::model_list",
                            error = %e,
                            "skipping a model/list entry this build cannot decode"
                        );
                    }
                }
            }
            match page.next_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => {
                    if skipped > 0 {
                        tracing::warn!(
                            target = "shared_codex_daemon::model_list",
                            skipped,
                            kept = models.len(),
                            "model/list returned entries this build cannot decode"
                        );
                    }
                    return Ok(models);
                }
            }
        }
        // The peer never cleared `nextCursor`. A partial catalog is never presented as a whole
        // one, which is also why `?` above abandons the pages already collected.
        Err(CalmError::CodexAppServer(format!(
            "model/list did not terminate its pagination within {MODEL_LIST_MAX_PAGES} pages"
        )))
    }

    /// `config/read` — the layer-merged effective config, narrowed to the model defaults;
    /// `cwd` selects the project layers.
    pub async fn config_read(
        &self,
        cwd: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<CodexConfig> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            if fake.reject_config_read.load(Ordering::SeqCst) {
                return Err(CalmError::CodexRefused(
                    "config/read failed: no such workspace (code -32602)".into(),
                ));
            }
            if let Some(config) = fake
                .config_read
                .lock()
                .expect("fake shared codex config-read mutex poisoned")
                .clone()
            {
                return Ok(config);
            }
        }
        let client = self.connected_client().await?;
        Ok(client.config_read(cwd, deadline).await?.config)
    }

    /// Drop this daemon's `thread_id -> card_id` attribution for Cards whose delete has already
    /// committed, so a reconnect does not resume a thread with no database owner. Takes
    /// `kernel_thread_start_serial` and `resume_replay_serial`; infallible on purpose.
    pub async fn forget_threads_for_deleted_cards(&self, card_ids: &HashSet<String>) -> usize {
        if card_ids.is_empty() {
            return 0;
        }
        let _start_guard = self.kernel_thread_start_serial.lock().await;
        // And the replay boundary, in this order: an in-flight `resume_cached_threads` may hold a
        // pre-delete snapshot.
        let _replay_guard = self.resume_replay_serial.lock().await;
        let mut dropped: Vec<(String, String)> = Vec::new();
        self.thread_cache.retain(|thread_id, card_id| {
            if card_ids.contains(card_id) {
                dropped.push((thread_id.clone(), card_id.clone()));
                false
            } else {
                true
            }
        });
        {
            let mut forgotten = self.forgotten_threads.lock().await;
            for (thread_id, _) in &dropped {
                forgotten.remember(thread_id);
            }
        }
        for (thread_id, card_id) in &dropped {
            tracing::info!(
                target = "shared_codex_daemon::forget_deleted_card_thread",
                %thread_id,
                %card_id,
                "dropped shared codex thread attribution for a deleted card"
            );
        }
        dropped.len()
    }

    /// Drop the deletion-time turn bookkeeping for threads a committed delete has sealed and
    /// quiesced. Deliberately takes no guard: every other writer of these maps uses `DashMap`'s
    /// per-entry locking only, and taking the start serial across a respawn self-deadlocks.
    pub fn forget_turn_state_for_deleted_threads(&self, thread_ids: &[String]) -> usize {
        let mut dropped = 0_usize;
        for thread_id in thread_ids {
            if self.sealed_turn_threads.remove(thread_id).is_some() {
                dropped += 1;
            }
            if self.active_turns.remove(thread_id).is_some() {
                dropped += 1;
            }
        }
        if dropped > 0 {
            tracing::info!(
                target = "shared_codex_daemon::forget_deleted_turn_state",
                threads = thread_ids.len(),
                dropped,
                "dropped deletion-time turn bookkeeping for a committed delete"
            );
        }
        dropped
    }

    /// The `(thread_id, card_id)` pairs `resume_cached_threads` will iterate; a snapshot, each
    /// pair re-validated under `resume_replay_serial` immediately before its resume RPC.
    fn resume_candidates(&self) -> Vec<(String, String)> {
        self.thread_cache
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    pub fn seal_turn_thread_for_deletion(&self, thread_id: &str) {
        self.sealed_turn_threads.insert(thread_id.to_string(), ());
    }

    pub fn unseal_turn_thread_after_rollback(&self, thread_id: &str) {
        self.sealed_turn_threads.remove(thread_id);
    }

    pub(crate) fn turn_thread_is_sealed(&self, thread_id: &str) -> bool {
        self.sealed_turn_threads.contains_key(thread_id)
    }

    /// `turn/steer` — hand `items` to the turn running on `thread_id`; codex refuses when
    /// `expected_turn_id` is not the active turn (`CalmError::CodexRefused`). No seal check and
    /// no `active_turns` write: a steer creates no turn.
    pub async fn turn_steer(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        items: Vec<InputItem>,
        client_user_message_id: Option<&str>,
    ) -> Result<TurnId> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            fake.steered_turns
                .lock()
                .expect("fake shared codex steered turns mutex poisoned")
                .push((
                    thread_id.to_string(),
                    expected_turn_id.to_string(),
                    items.clone(),
                    client_user_message_id.map(ToOwned::to_owned),
                ));
            let hook = fake
                .turn_steer_return_hook
                .lock()
                .expect("fake shared codex turn-steer hook mutex poisoned")
                .take();
            if let Some(hook) = hook {
                hook.entered.notify_one();
                hook.release.notified().await;
            }
            let scripted = fake
                .reject_turn_steer
                .lock()
                .expect("fake shared codex reject-steer mutex poisoned")
                .clone();
            if let Some(message) = scripted {
                return Err(CalmError::CodexRefused(message));
            }
            if fake.fail_turn_steer.load(Ordering::SeqCst) {
                return Err(CalmError::CodexAppServer(
                    "request turn/steer timed out".into(),
                ));
            }
            return match self.active_turn_id_for_thread(thread_id) {
                None => Err(CalmError::CodexRefused(
                    "turn/steer failed: no active turn to steer (code -32600)".into(),
                )),
                Some(active) if active != expected_turn_id => {
                    Err(CalmError::CodexRefused(format!(
                        "turn/steer failed: expected active turn id `{expected_turn_id}` but \
                         found `{active}` (code -32600)"
                    )))
                }
                Some(active) => Ok(active),
            };
        }
        let client = self.connected_client().await?;
        let steered = client
            .turn_steer(thread_id, expected_turn_id, items, client_user_message_id)
            .await?;
        Ok(steered.turn_id)
    }

    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            fake.interrupted_turns
                .lock()
                .expect("fake shared codex interrupted turns mutex poisoned")
                .push((thread_id.to_string(), turn_id.to_string()));
            if fake.fail_turn_interrupt.load(Ordering::SeqCst) {
                return Err(CalmError::CodexAppServer(
                    "fixture: turn/interrupt failed".into(),
                ));
            }
            self.active_turns
                .remove_if(thread_id, |_, active| active == turn_id);
            return Ok(());
        }
        let client = self.connected_client().await?;
        client.turn_interrupt(thread_id, turn_id).await
    }

    pub fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<TurnId> {
        self.active_turns
            .get(thread_id)
            .map(|entry| entry.value().clone())
    }

    pub async fn interrupt_active_turn(&self, thread_id: &str) -> Result<()> {
        let Some(turn_id) = self
            .active_turns
            .get(thread_id)
            .map(|entry| entry.value().clone())
        else {
            return Ok(());
        };
        self.turn_interrupt(thread_id, &turn_id).await?;
        self.active_turns
            .remove_if(thread_id, |_, active| active == &turn_id);
        Ok(())
    }

    pub async fn interrupt_active_turn_for_card(&self, card_id: &str) -> Result<()> {
        crate::isolated_codex::lookup::require_shared_card(self.repo.as_ref(), card_id).await?;
        let Some(thread_id) = resolve_active_thread_for_card(self.repo.as_ref(), card_id).await?
        else {
            return Ok(());
        };
        self.interrupt_active_turn(&thread_id).await
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }

    pub fn is_running(&self) -> bool {
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            return true;
        }
        self.core
            .try_lock()
            .is_ok_and(|core| matches!(core.state, SupervisorState::Running { .. }))
    }

    pub fn remote_uri(&self) -> String {
        format!("unix://{}", self.sock.display())
    }

    /// Wall-clock ms of the most recent successful daemon (re)connect; `0` before the first connect.
    pub fn daemon_connected_at_ms(&self) -> calm_types::runtime::TimestampMs {
        self.daemon_connected_at_ms.load(Ordering::SeqCst)
    }

    pub fn mark_needs_respawn(&self) {
        self.needs_respawn_on_next_thread_start
            .store(true, Ordering::SeqCst);
        // The settings-change path doubles as the heal loop's slow-lane wake. `notify_one` stores
        // a permit, so a nudge fired while the loop is mid-round is not lost.
        self.heal_nudge.notify_one();
    }

    pub fn status_snapshot(&self) -> SharedDaemonStatus {
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            return SharedDaemonStatus {
                state: SharedDaemonState::Running,
                sock: self.sock.display().to_string(),
                codex_home: self.home.path().display().to_string(),
                runtime: None,
                cached_threads: self.thread_cache.len(),
                pending_count: self
                    .pending_codex_threads_handle
                    .as_ref()
                    .map(|pending| pending.pending_count_snapshot())
                    .unwrap_or(0),
                restart_count: self.restart_count.load(Ordering::SeqCst),
                last_error: None,
            };
        }
        let (state, runtime, last_error) = self
            .core
            .try_lock()
            .map(|core| {
                let runtime = match &core.state {
                    SupervisorState::Running { runtime, .. } => Some(runtime.clone()),
                    _ => None,
                };
                (
                    core.state.as_shared_daemon_state(),
                    runtime,
                    core.state.last_error().map(String::from),
                )
            })
            .unwrap_or((SharedDaemonState::Failed, None, None));
        SharedDaemonStatus {
            state,
            sock: self.sock.display().to_string(),
            codex_home: self.home.path().display().to_string(),
            runtime,
            cached_threads: self.thread_cache.len(),
            pending_count: self
                .pending_codex_threads_handle
                .as_ref()
                .map(|pending| pending.pending_count_snapshot())
                .unwrap_or(0),
            restart_count: self.restart_count.load(Ordering::SeqCst),
            last_error,
        }
    }

    pub fn cached_card_for_thread(&self, thread_id: &str) -> Option<String> {
        self.thread_cache.get(thread_id).map(|v| v.value().clone())
    }
}

impl SharedCodexAppServer {
    pub fn effective_proxy_env(settings_value: Option<&str>, env_keys: &[&str]) -> Option<String> {
        Self::effective_proxy_env_from(settings_value, env_keys, |key| std::env::var(key).ok())
    }

    pub fn effective_proxy_env_from(
        settings_value: Option<&str>,
        env_keys: &[&str],
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Option<String> {
        if let Some(v) = settings_value {
            return Some(v.to_string());
        }
        env_keys
            .iter()
            .find_map(|key| lookup(key).filter(|v| !v.is_empty()))
    }

    pub fn compute_env_signature(
        ingest_url: &str,
        http_proxy: Option<&str>,
        https_proxy: Option<&str>,
        kernel_bin_dir: &Path,
    ) -> String {
        let mut h = Sha256::new();
        // Schema-version salt: the first boot of an upgraded binary mismatches every pre-upgrade
        // signature, so the takeover path replaces a daemon spawned with the old inherited env.
        h.update(b"env-schema-v3:1784|");
        // The PATH the daemon's exec-shells resolve `neige` through leads with this dir.
        h.update(kernel_bin_dir.as_os_str().as_encoded_bytes());
        h.update(b"|");
        h.update(ingest_url.as_bytes());
        h.update(b"|");
        h.update(http_proxy.unwrap_or_default().as_bytes());
        h.update(b"|");
        h.update(https_proxy.unwrap_or_default().as_bytes());
        let hex = hex::encode(h.finalize());
        hex[..16].to_string()
    }

    /// One settings snapshot per spawn: the child's proxy env AND the persisted signature must
    /// derive from the SAME settings read.
    async fn load_spawn_env_snapshot(&self) -> Result<SpawnEnvSnapshot> {
        let settings = load_settings(self.repo.as_ref()).await?;
        let kernel_path = crate::kernel_bin_path::kernel_led_path().map_err(|error| {
            CalmError::Internal(format!("shared codex app-server PATH: {error}"))
        })?;
        Ok(SpawnEnvSnapshot {
            kernel_path,
            http_proxy: Self::effective_proxy_env(
                settings.http_proxy.as_deref(),
                &["HTTP_PROXY", "http_proxy"],
            ),
            https_proxy: Self::effective_proxy_env(
                settings.https_proxy.as_deref(),
                &["HTTPS_PROXY", "https_proxy"],
            ),
        })
    }

    fn env_signature_for_snapshot(&self, snapshot: &SpawnEnvSnapshot) -> String {
        Self::compute_env_signature(
            &self.ingest_url,
            snapshot.http_proxy.as_deref(),
            snapshot.https_proxy.as_deref(),
            &snapshot.kernel_path.bin_dir,
        )
    }

    async fn current_env_signature(&self) -> Result<String> {
        Ok(self.env_signature_for_snapshot(&self.load_spawn_env_snapshot().await?))
    }

    /// Settings-first, parent-env-fallback proxy resolution as explicit (UPPER, lower, value)
    /// pairs; with `env_clear()` the fallback must be SET explicitly.
    pub fn resolved_proxy_env_pairs(
        http_settings: Option<&str>,
        https_settings: Option<&str>,
        lookup: impl Fn(&str) -> Option<String> + Copy,
    ) -> Vec<(&'static str, &'static str, String)> {
        let mut pairs = Vec::new();
        if let Some(v) =
            Self::effective_proxy_env_from(http_settings, &["HTTP_PROXY", "http_proxy"], lookup)
                .filter(|v| !v.is_empty())
        {
            pairs.push(("HTTP_PROXY", "http_proxy", v));
        }
        if let Some(v) =
            Self::effective_proxy_env_from(https_settings, &["HTTPS_PROXY", "https_proxy"], lookup)
                .filter(|v| !v.is_empty())
        {
            pairs.push(("HTTPS_PROXY", "https_proxy", v));
        }
        pairs
    }

    /// The child env is a pure function of typed config: `env_clear()` plus exactly
    /// [`SPAWN_ENV_PASSTHROUGH`], the computed keys, and (fixture builds only) the fake-codex channel.
    fn apply_spawn_env(&self, cmd: &mut Command, snapshot: &SpawnEnvSnapshot) {
        cmd.env_clear();
        for key in SPAWN_ENV_PASSTHROUGH {
            // var_os: a non-UTF8 value must pass through, not be silently dropped
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }

        // Fixture channel (test-only passthrough). Compiled out of production builds — these
        // names must NEVER join the prod `SPAWN_ENV_PASSTHROUGH` const.
        #[cfg(feature = "fixtures")]
        for (key, value) in std::env::vars_os() {
            let fixture_key = key
                .to_str()
                .is_some_and(|k| k.starts_with("FAKE_CODEX_") || k == "NEIGE_OSC_TRACE_PATH");
            if fixture_key {
                cmd.env(&key, value);
            }
        }

        // Planner and Worker exec-shells inherit this PATH; MCP child commands are NOT
        // which-resolved on unix, so they resolve through it too.
        cmd.env("PATH", &snapshot.kernel_path.path)
            .env("CODEX_HOME", self.home.path())
            .env("NEIGE_CALM_BASE_URL", &self.ingest_url);

        // The snapshot values are already resolved; the lookup here is inert.
        for (upper, lower, value) in Self::resolved_proxy_env_pairs(
            snapshot.http_proxy.as_deref(),
            snapshot.https_proxy.as_deref(),
            |_| None,
        ) {
            cmd.env(upper, &value).env(lower, value);
        }
    }
}

/// A single-settings-read snapshot of the spawn-relevant runtime settings.
struct SpawnEnvSnapshot {
    kernel_path: crate::kernel_bin_path::KernelLedPath,
    http_proxy: Option<String>,
    https_proxy: Option<String>,
}

/// Outcome of the takeover connect probe: `running` rows keep the single-attempt fast-fail
/// (boot must not stall for a broken daemon); `starting` rows get the bounded readiness window.
enum AdoptProbe {
    /// The socket answered `initialize` — continue into the adoption body.
    Connected((CodexAppServer, crate::codex_appserver::NotificationStream)),
    /// Single-attempt probe failed against a live verified daemon —
    /// handshake-failure reap, then fall through to spawn.
    HandshakeFailed(String),
    /// The child exited during the readiness window — fall through to
    /// spawn with no reap (the stale-socket reap before spawn covers a
    /// half-bound socket).
    ChildExited,
    /// The remaining window lapsed with the child alive and unbound —
    /// graceful reap (defect-1 helper), then fall through to spawn.
    WindowLapsed { last_error: String },
}

impl SharedCodexAppServer {
    async fn connected_client(&self) -> Result<Arc<CodexAppServer>> {
        self.running_client()
            .await
            .ok_or_else(|| CalmError::CodexAppServer("shared app-server is not connected".into()))
    }

    /// Caller MUST hold `transition_serial`.
    async fn try_takeover_live(
        self: &Arc<Self>,
        record: &crate::db::SharedCodexDaemonRecord,
    ) -> Result<bool> {
        let (Some(pid), Some(pgid), Some(start_time), Some(boot_id), Some(started_at)) = (
            record.pid,
            record.pgid,
            record.process_start_time,
            record.boot_id.clone(),
            record.started_at,
        ) else {
            return Ok(false);
        };
        if matches!(
            SharedDaemonState::from_db_str(&record.state),
            SharedDaemonState::Idle | SharedDaemonState::Failed
        ) {
            return Ok(false);
        }
        if !verify_owned_pid(pid, start_time, &boot_id) {
            tracing::warn!(
                target = "shared_codex_daemon::restart",
                pid,
                pgid,
                "shared codex app-server persisted pid is stale"
            );
            return Ok(false);
        }
        // Terminalize a settings-read failure here (in-memory Failed + heal armed), else the serial
        // is released with the state stranded `Restarting`. DB row deliberately untouched: it still
        // truthfully names a live VERIFIED daemon.
        let current_env_signature = match self.current_env_signature().await {
            Ok(signature) => signature,
            Err(e) => {
                let msg =
                    format!("failed reading runtime settings for takeover env comparison: {e}");
                self.fail_in_memory_and_arm_heal(msg.clone(), FailureClass::Persistent)
                    .await;
                return Err(CalmError::CodexAppServer(msg));
            }
        };
        // Signature mismatch + verified healthy daemon ⇒ ADOPT-AND-DRAIN, never reap-for-respawn:
        // the v2 salt mismatches on every first boot after an upgrade, and every mint path crosses
        // the needs_respawn drain boundary; only `turn_start` on an EXISTING thread does not.
        let signature_mismatch =
            record.daemon_env_signature.as_deref() != Some(current_env_signature.as_str());
        if signature_mismatch {
            tracing::warn!(
                target: "shared_codex_daemon::takeover_env_changed",
                pid,
                pgid,
                persisted = ?record.daemon_env_signature,
                current = %current_env_signature,
                "shared daemon was spawned with stale env signature; \
                 adopting and marking for drain at the next thread-start boundary"
            );
        }
        let Some(sock_path) = &record.sock_path else {
            return Ok(false);
        };
        let sock = PathBuf::from(sock_path);
        // A `starting` row names a child still inside its cold-start budget (mid-backfill), so it
        // gets the REMAINING readiness window instead of the instant handshake fast-fail.
        let probe = if matches!(
            SharedDaemonState::from_db_str(&record.state),
            SharedDaemonState::Starting
        ) {
            // ms-domain saturating math anchored to the PERSISTED `started_at`: clock skew degenerates
            // to a full or zero window; repeated boots never re-arm the window.
            let elapsed = Duration::from_millis(now_ms().saturating_sub(started_at).max(0) as u64);
            let remaining = self.start_timeout.saturating_sub(elapsed);
            self.poll_adopt_initialized(&sock, pid, start_time, &boot_id, remaining)
                .await
        } else {
            // The deadline for this arm lives at the transport (`CodexAppServer::connect`'s
            // `CONNECT_TIMEOUT`); a silent peer lands in `HandshakeFailed`, which reaps and relaunches.
            match connect_initialized(&sock).await {
                Ok(pair) => AdoptProbe::Connected(pair),
                Err(e) => AdoptProbe::HandshakeFailed(e.to_string()),
            }
        };
        match probe {
            AdoptProbe::Connected((client, notifications)) => {
                let client = Arc::new(client);
                let runtime = SharedDaemonRuntime {
                    pid,
                    pgid,
                    boot_id,
                    process_start_time: start_time,
                    started_at,
                };
                self.install_client(client.clone(), notifications).await?;
                let watcher_self = Arc::downgrade(self);
                let watcher_runtime = runtime.clone();
                let handle = tokio::spawn(async move {
                    Self::watch_taken_over_pid(watcher_self, watcher_runtime).await;
                });
                let adopted = runtime.clone();
                self.start_new_process_typestate(|_| async move {
                    Ok(LaunchedSharedDaemon {
                        child: None,
                        client,
                        runtime,
                        watcher: SupervisorWatcher {
                            kind: WatcherKind::TakenOverPid { pid },
                            handle,
                        },
                    })
                })
                .await?;
                // Takeover success re-stamps `running` with the adopted tuple but keeps the OLD persisted
                // signature: the adoption path MUST NOT write the current signature anywhere, since
                // `needs_respawn` is in-memory and the next boot must re-detect the mismatch.
                if let Err(e) = self
                    .repo
                    .shared_daemon_runtime_set(SharedCodexDaemonUpdate {
                        state: SharedDaemonState::Running.as_db_str().to_string(),
                        pid: Some(adopted.pid),
                        pgid: Some(adopted.pgid),
                        sock_path: record.sock_path.clone(),
                        codex_home_path: record.codex_home_path.clone(),
                        process_start_time: Some(adopted.process_start_time),
                        boot_id: Some(adopted.boot_id.clone()),
                        started_at: Some(adopted.started_at),
                        last_error: record.last_error.clone(),
                        increment_restart_count: false,
                        daemon_env_signature: record.daemon_env_signature.clone(),
                    })
                    .await
                {
                    tracing::warn!(
                        target: "shared_codex_daemon::start",
                        pid = adopted.pid,
                        error = %e,
                        "takeover succeeded but re-stamping the running row failed"
                    );
                } else {
                    self.failed_persist_dedup.clear();
                }
                if signature_mismatch {
                    // Arm the drain AFTER the adoption is installed (re-stamp Ok or Err alike).
                    self.mark_needs_respawn();
                }
                Ok(true)
            }
            AdoptProbe::HandshakeFailed(e) => {
                tracing::warn!(
                    target: "shared_codex_daemon::stop",
                    pid,
                    pgid,
                    error = %e,
                    "takeover handshake failed against verified daemon; reaping pgid before relaunch"
                );
                reap_verified_process_group(pid, pgid, start_time, &boot_id, self.stop_grace).await;
                Ok(false)
            }
            AdoptProbe::ChildExited => Ok(false),
            AdoptProbe::WindowLapsed { last_error } => {
                tracing::warn!(
                    target: "shared_codex_daemon::stop",
                    pid,
                    pgid,
                    error = %last_error,
                    "starting child never bound within its remaining readiness \
                     window; reaping gracefully before relaunch"
                );
                // The same wall-clock deadline the child's own supervisor would have enforced, just by a
                // successor process — graceful, never an instant SIGKILL.
                reap_verified_process_group(pid, pgid, start_time, &boot_id, self.stop_grace).await;
                Ok(false)
            }
        }
    }

    /// THE atomic replace transition: every process transition funnels through here under a
    /// single continuously-held `transition_serial`, released only after the terminal
    /// Running/Failed write. Worst-case hold ≈ readiness window + `stop_grace` + `start_timeout`.
    pub(crate) async fn transition_replace(
        self: &Arc<Self>,
        reason: &str,
        pre: ReplacePrecondition,
        increment_restart_count: bool,
        last_error: Option<String>,
    ) -> Result<ReplaceOutcome> {
        let via = {
            let serial = Arc::clone(&self.transition_serial).lock_owned().await;
            let (running, outgoing_generation) = {
                let mut core = self.core.lock().await;
                match pre {
                    ReplacePrecondition::Always => {}
                    ReplacePrecondition::GenerationIs(generation) => {
                        // Generation equality alone is not staleness proof: it bumps only when a Running
                        // incarnation is INSTALLED, so a failed intervening transition leaves it unchanged.
                        // Anything but Running-with-this-generation means another transition consumed it.
                        if core.generation != generation
                            || !matches!(core.state, SupervisorState::Running { .. })
                        {
                            return Ok(ReplaceOutcome::PreconditionFailed);
                        }
                    }
                    ReplacePrecondition::NotRunning => {
                        if matches!(core.state, SupervisorState::Running { .. }) {
                            return Ok(ReplaceOutcome::PreconditionFailed);
                        }
                    }
                }
                core.attempts = core.attempts.saturating_add(1);
                let attempts = core.attempts;
                let prev_pid = match &core.state {
                    SupervisorState::Running { runtime, .. } => Some(runtime.pid),
                    _ => None,
                };
                let old_state = std::mem::replace(
                    &mut core.state,
                    SupervisorState::Restarting {
                        prev_pid,
                        reason: reason.to_string(),
                        attempts,
                    },
                );
                let parts = match old_state {
                    SupervisorState::Running {
                        child,
                        runtime,
                        watcher,
                        ..
                    } => Some(RunningProcessParts {
                        child,
                        runtime,
                        watcher,
                    }),
                    _ => None,
                };
                (parts, core.generation)
            };
            // Transition ENTRY: invalidate readiness NOW with the outgoing generation, else
            // claim-boundary consumers would accept a transitional daemon. Every arm re-publishes the
            // terminal value before the serial is released.
            self.readiness.send_replace(DaemonReadiness {
                generation: outgoing_generation,
                running: false,
            });
            // Fixtures-only interleaving gate: park here so tests can
            // observe the entry-time window deterministically.
            #[cfg(feature = "fixtures")]
            drop(self.transition_entry_gate.lock().await);
            self.reap_current_child_or_runtime(running).await;
            self.start_body_locked(serial, increment_restart_count, last_error)
                .await?
        };
        self.resume_cached_threads(via.resume_mode()).await;
        Ok(ReplaceOutcome::Replaced)
    }

    /// The ONE shared start body: guard verify → persisted-record reconciliation → takeover
    /// probe → spawn. The by-value `serial` token is dropped only after the terminal write; the
    /// spawn path moves it into the detached transition task.
    async fn start_body_locked(
        self: &Arc<Self>,
        serial: tokio::sync::OwnedMutexGuard<()>,
        increment_restart_count: bool,
        last_error: Option<String>,
    ) -> Result<StartedVia> {
        // Boot guard: refuse before touching the process, and never strand a previously-live
        // polluted daemon running unsupervised.
        if let Err(guard_err) = self.home.verify_expected_mcp_servers(EXPECTED_MCP_SERVERS) {
            let msg = format!("refusing to launch shared codex app-server: {guard_err}");
            self.record_guard_refusal(&msg).await;
            return Err(CalmError::CodexAppServer(msg));
        }
        // A `failed` row with ANY identity column present is an unreconciled possible survivor;
        // the spawn path is unreachable until the per-shape resolution proves absence.
        let record = match self.repo.shared_daemon_runtime_get().await {
            Ok(record) => record,
            Err(e) => {
                // Nothing trustworthy to write (the read itself failed):
                // keep class Unreconciled in memory only; the next heal
                // round re-reads.
                let msg = format!("unreconciled: failed reading persisted daemon record: {e}");
                self.fail_in_memory_and_arm_heal(msg.clone(), FailureClass::Unreconciled)
                    .await;
                return Err(CalmError::CodexAppServer(msg));
            }
        };
        if failed_row_identity_present(&record) {
            match self.resolve_persisted_shape(&record).await {
                ShapeResolution::ProvenAbsent => {
                    // Durable proof-of-absence transition: NULL the identity
                    // columns so the row classifies SafeToRetry even if the
                    // spawn below never completes.
                    let proven_error = record
                        .last_error
                        .as_deref()
                        .map(strip_unreconciled_prefix)
                        .unwrap_or("unreconciled survivor proven absent")
                        .to_string();
                    self.persist_failed_deduped(
                        &proven_error,
                        FailureClass::Persistent,
                        FailedIdentity::proven_absent(&self.sock, self.home.path()),
                    )
                    .await;
                }
                ShapeResolution::Unreconciled { needs_operator } => {
                    let msg = unreconciled_message(&record, needs_operator);
                    self.persist_failed_deduped(
                        &msg,
                        FailureClass::Unreconciled,
                        FailedIdentity::retained(&record),
                    )
                    .await;
                    self.fail_in_memory_and_arm_heal(msg.clone(), FailureClass::Unreconciled)
                        .await;
                    return Err(CalmError::CodexAppServer(msg));
                }
            }
        }
        // `try_takeover_live` skips Idle/Failed rows itself — a refused
        // survivor is never adopted.
        if self.try_takeover_live(&record).await? {
            return Ok(StartedVia::Takeover);
        }
        self.spawn_process_transition(serial, increment_restart_count, last_error)
            .await?;
        Ok(StartedVia::Spawn)
    }

    /// Boot-guard refusal arm (home pollution): a verified owned survivor is reaped; anything
    /// unprovable stays unreconciled with the identity tuple RETAINED as the durable marker.
    async fn record_guard_refusal(self: &Arc<Self>, guard_error: &str) {
        let record = match self.repo.shared_daemon_runtime_get().await {
            Ok(record) => record,
            Err(e) => {
                tracing::warn!(
                    target: "shared_codex_daemon::stop",
                    error = %e,
                    "guard refusal: failed reading persisted daemon runtime; skipping reconciliation"
                );
                self.fail_in_memory_and_arm_heal(
                    format!("unreconciled: {guard_error} (persisted record read failed: {e})"),
                    FailureClass::Unreconciled,
                )
                .await;
                return;
            }
        };
        match self.resolve_persisted_shape(&record).await {
            ShapeResolution::ProvenAbsent => {
                // No owned process (or the verified reap completed): persist
                // the refusal with identity NULLed so DB truth reflects it.
                self.persist_failed_deduped(
                    guard_error,
                    FailureClass::Persistent,
                    FailedIdentity::proven_absent(&self.sock, self.home.path()),
                )
                .await;
                self.fail_in_memory_and_arm_heal(guard_error.to_string(), FailureClass::Persistent)
                    .await;
            }
            ShapeResolution::Unreconciled { needs_operator } => {
                let msg = format!("{}{guard_error}", unreconciled_prefix(needs_operator));
                self.persist_failed_deduped(
                    &msg,
                    FailureClass::Unreconciled,
                    FailedIdentity::retained(&record),
                )
                .await;
                self.fail_in_memory_and_arm_heal(msg, FailureClass::Unreconciled)
                    .await;
            }
        }
    }

    /// Exhaustive per-shape resolution over every persisted identity combination. A destructive
    /// group reap is allowed ONLY in the full-tuple pgid==pid shape — never signal a pgid we did
    /// not create, never signal a bare pid.
    async fn resolve_persisted_shape(
        &self,
        record: &crate::db::SharedCodexDaemonRecord,
    ) -> ShapeResolution {
        let Some(pid) = record.pid else {
            if record.pgid.is_none()
                && record.process_start_time.is_none()
                && record.boot_id.is_none()
            {
                // Fully-empty tuple: the normal "no daemon was ever
                // persisted" state.
                return ShapeResolution::ProvenAbsent;
            }
            // Shape (b): identity fragments without a pid name no process at all; operator
            // remediation only.
            tracing::warn!(
                target: "shared_codex_daemon::stop",
                pgid = ?record.pgid,
                process_start_time = ?record.process_start_time,
                boot_id = ?record.boot_id,
                "persisted daemon record has identity fragments but no pid; operator must clear the identity columns"
            );
            return ShapeResolution::Unreconciled {
                needs_operator: true,
            };
        };
        let (Some(start_time), Some(boot_id)) = (record.process_start_time, record.boot_id.clone())
        else {
            // Shape (a): pid without a complete verification pair — ownership unprovable (the pid may
            // be recycled), so never reap or signal; `/proc` existence (zombies INCLUDED) is the only safe probe.
            if proc_pid_present(pid) {
                tracing::warn!(
                    target: "shared_codex_daemon::stop",
                    pid,
                    "persisted daemon record has a pid with an incomplete verification pair and the pid is present; staying unreconciled (never signaling a bare pid)"
                );
                return ShapeResolution::Unreconciled {
                    needs_operator: false,
                };
            }
            return ShapeResolution::ProvenAbsent;
        };
        if !verify_owned_pid(pid, start_time, &boot_id) {
            // Natural death (or the identity matches no live process). A verified-but-ZOMBIE leader
            // falls through to the pgid arms: live descendants may remain in its group.
            return ShapeResolution::ProvenAbsent;
        }
        match record.pgid {
            Some(pgid) if pgid == pid => {
                // Full tuple honoring the spawn invariant (`process_group(0)`
                // ⇒ pgid == pid): verified group reap, then absence is
                // claimed only if the post-reap check confirms death.
                tracing::warn!(
                    target: "shared_codex_daemon::stop",
                    pid,
                    pgid,
                    "reaping verified persisted daemon survivor"
                );
                reap_verified_process_group(pid, pgid, start_time, &boot_id, self.stop_grace).await;
                if survivor_alive_after_group_reap(pid, start_time, &boot_id) {
                    tracing::warn!(
                        target: "shared_codex_daemon::stop",
                        pid,
                        pgid,
                        "persisted daemon still verifies alive after the reap; staying unreconciled"
                    );
                    ShapeResolution::Unreconciled {
                        needs_operator: false,
                    }
                } else {
                    ShapeResolution::ProvenAbsent
                }
            }
            Some(pgid) => {
                // Corrupt record: never signal a pgid we did not create
                // (risk of killing unrelated processes). Self-resolves on
                // natural death via verify-false above.
                tracing::warn!(
                    target: "shared_codex_daemon::stop",
                    pid,
                    pgid,
                    "persisted pgid does not match pid (spawn invariant pgid == pid); staying unreconciled without signaling"
                );
                ShapeResolution::Unreconciled {
                    needs_operator: false,
                }
            }
            None => {
                // Triple complete but no pgid: signaling the bare pid is not a group reap and risks
                // orphaning grandchildren mid-tree. Self-resolves on death.
                tracing::warn!(
                    target: "shared_codex_daemon::stop",
                    pid,
                    "persisted daemon record verifies alive but has no pgid; staying unreconciled (no valid pgid to target)"
                );
                ShapeResolution::Unreconciled {
                    needs_operator: false,
                }
            }
        }
    }

    /// Flip the in-memory state to Failed and arm the heal loop. The core
    /// mutex is held only to flip state — no DB I/O under it, ever.
    async fn fail_in_memory_and_arm_heal(
        self: &Arc<Self>,
        last_error: String,
        class: FailureClass,
    ) {
        let generation = {
            let mut core = self.core.lock().await;
            core.state = SupervisorState::Failed {
                last_error,
                class,
                since: Instant::now(),
            };
            core.generation
        };
        // Readiness stamped on every terminal Failed.
        self.readiness.send_replace(DaemonReadiness {
            generation,
            running: false,
        });
        self.schedule_heal();
    }

    /// The single Failed DB writer, with post-Ok dedup. Persist errors are logged, never
    /// swallowed, and never mask the original failure.
    async fn persist_failed_deduped(
        &self,
        last_error: &str,
        class: FailureClass,
        identity: FailedIdentity,
    ) {
        if self
            .failed_persist_dedup
            .should_skip(last_error, class, &identity)
        {
            tracing::debug!(
                target: "shared_codex_daemon::heal",
                last_error,
                "identical consecutive daemon failure; skipping duplicate Failed persist"
            );
            return;
        }
        tracing::warn!(
            target: "shared_codex_daemon::heal",
            last_error,
            class = ?class,
            "persisting failed shared codex daemon state"
        );
        match self
            .repo
            .shared_daemon_runtime_set(SharedCodexDaemonUpdate {
                state: SharedDaemonState::Failed.as_db_str().to_string(),
                pid: identity.pid,
                pgid: identity.pgid,
                sock_path: identity.sock_path.clone(),
                codex_home_path: identity.codex_home_path.clone(),
                process_start_time: identity.process_start_time,
                boot_id: identity.boot_id.clone(),
                started_at: identity.started_at,
                last_error: Some(last_error.to_string()),
                increment_restart_count: false,
                daemon_env_signature: identity.daemon_env_signature.clone(),
            })
            .await
        {
            Ok(()) => {
                // Post-Ok only: a failed write must be retried next round.
                self.failed_persist_dedup
                    .note_written(last_error.to_string(), class, identity);
            }
            Err(e) => {
                tracing::error!(
                    target: "shared_codex_daemon::heal",
                    error = %e,
                    last_error,
                    "failed persisting failed daemon record"
                );
            }
        }
    }

    /// Spawn-transition body, DETACHED from its caller: the whole typestate unit runs inside
    /// ONE `tokio::spawn` task owning the serial token, so caller cancellation cannot strand a
    /// half-done transition. `tokio::spawn` is synchronous, so the task exists before the first await.
    async fn spawn_process_transition(
        self: &Arc<Self>,
        serial: tokio::sync::OwnedMutexGuard<()>,
        increment_restart_count: bool,
        last_error: Option<String>,
    ) -> Result<()> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let this = Arc::clone(self);
        let task = tokio::spawn(async move {
            let launch_this = Arc::clone(&this);
            let result = this
                .start_new_process_typestate(move |_| async move {
                    launch_this
                        .launch_spawned_process(increment_restart_count, last_error)
                        .await
                })
                .await;
            // Release the serial BEFORE publishing the result so an observer never races a still-held serial.
            drop(serial);
            // Send failure = the caller was cancelled after detachment;
            // deliberately ignored — the transition is already terminal.
            let _ = result_tx.send(result);
        });
        #[cfg(feature = "fixtures")]
        {
            *self
                .detached_spawn_task
                .lock()
                .expect("detached spawn task seam mutex poisoned") = Some(task.abort_handle());
        }
        #[cfg(not(feature = "fixtures"))]
        drop(task);
        match result_rx.await {
            Ok(result) => result,
            // The sender dropped without sending: task panic or runtime teardown. This observer CANNOT
            // terminalize (the task owns the serial); the durable row still names the child, so the
            // next transition or boot reconciliation resolves it.
            Err(_) => Err(CalmError::CodexAppServer(format!(
                "{DETACHED_SPAWN_RESULT_LOST} (task panic or runtime teardown)"
            ))),
        }
    }

    async fn launch_spawned_process(
        self: &Arc<Self>,
        increment_restart_count: bool,
        last_error: Option<String>,
    ) -> Result<LaunchedSharedDaemon> {
        // Boot guard on EVERY spawn: pollution written while running is caught at the next launch.
        // The ConfigLock is released between this verification and the exec; accepted residual.
        self.home
            .verify_expected_mcp_servers(EXPECTED_MCP_SERVERS)
            .map_err(|e| {
                CalmError::CodexAppServer(format!(
                    "refusing to launch shared codex app-server: {e}"
                ))
            })?;
        // ONE settings read per spawn: the child env and the persisted signature both derive from it.
        let spawn_env_snapshot = self.load_spawn_env_snapshot().await?;
        std::fs::create_dir_all(self.sock.parent().unwrap_or_else(|| Path::new(".")))?;
        std::fs::create_dir_all(&self.log_dir)?;
        self.remove_stale_socket_before_spawn().await?;

        let listen = format!("unix://{}", self.sock.display());
        let stdout = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_dir.join("stdout.log"))?;
        let stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_dir.join("stderr.log"))?;

        let mut cmd = Command::new(&self.codex_bin);
        // Deliberately NO `.kill_on_drop(true)`: tokio would SIGKILL the child on every `Child`
        // drop regardless of the guard's Drop body, and that instant SIGKILL is the lease-armer
        // that livelocked production. Termination happens ONLY via the graceful helpers.
        cmd.arg("app-server")
            .arg("--listen")
            .arg(&listen)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .process_group(0);
        self.apply_spawn_env(&mut cmd, &spawn_env_snapshot);
        let child = cmd.spawn().map_err(|e| {
            CalmError::CodexAppServer(format!("spawn shared codex app-server: {e}"))
        })?;
        let pid = child
            .id()
            .and_then(|p| i32::try_from(p).ok())
            .ok_or_else(|| {
                CalmError::CodexAppServer("shared app-server spawned without pid".into())
            })?;
        let pgid = pid;
        let process_start_time = read_proc_start_time(pid).unwrap_or(0);
        let boot_id = read_boot_id().unwrap_or_default();
        let started_at = now_ms();
        let daemon_env_signature = self.env_signature_for_snapshot(&spawn_env_snapshot);
        let runtime = SharedDaemonRuntime {
            pid,
            pgid,
            boot_id: boot_id.clone(),
            process_start_time,
            started_at,
        };
        let mut spawn_guard = SpawnedChildGuard::new(child, pgid);

        // The fallible tail: every ordinary error path awaits a GRACEFUL reap of the guarded child
        // (full stop grace, even for the cold-start-deadline miss) while still holding the serial.
        let launch = async {
            self.persist_runtime_starting(
                &runtime,
                last_error.clone(),
                daemon_env_signature.clone(),
            )
            .await?;
            let pair = self.poll_connect_initialized(&mut spawn_guard).await?;
            self.repo
                .shared_daemon_runtime_set(SharedCodexDaemonUpdate {
                    state: SharedDaemonState::Running.as_db_str().to_string(),
                    pid: Some(pid),
                    pgid: Some(pgid),
                    sock_path: Some(self.sock.display().to_string()),
                    codex_home_path: Some(self.home.path().display().to_string()),
                    process_start_time: Some(process_start_time),
                    boot_id: Some(boot_id.clone()),
                    started_at: Some(started_at),
                    last_error: last_error.clone(),
                    increment_restart_count,
                    daemon_env_signature: Some(daemon_env_signature.clone()),
                })
                .await?;
            Ok(pair)
        };
        let (client, notifications) = match launch.await {
            Ok(pair) => pair,
            Err(err) => {
                spawn_guard.reap_graceful(self.stop_grace).await;
                return Err(err);
            }
        };
        // If the effective signature STILL matches the one just persisted, any pending
        // `needs_respawn` drain is already satisfied by this very process — clear it so the next
        // mint doesn't pay a redundant cold start. The RE-READ preserves marks from later settings changes.
        if self
            .current_env_signature()
            .await
            .is_ok_and(|current| current == daemon_env_signature)
            && self
                .needs_respawn_on_next_thread_start
                .swap(false, Ordering::AcqRel)
        {
            tracing::info!(
                target = "shared_codex_daemon::restart",
                "cleared pending settings-drain: the fresh spawn already carries the current env signature"
            );
        }
        let client = Arc::new(client);
        self.install_client(client.clone(), notifications).await?;
        let child = spawn_guard.disarm();
        let watcher_self = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            Self::watch_spawned_child(watcher_self).await;
        });
        self.restart_backoff.note_relaunch_now();
        if increment_restart_count {
            self.restart_count.fetch_add(1, Ordering::SeqCst);
        }
        tracing::info!(
            target = "shared_codex_daemon::start",
            boot_id = %boot_id,
            pgid,
            sock = %self.sock.display(),
            home = %self.home.path().display(),
            "shared codex app-server running"
        );
        Ok(LaunchedSharedDaemon {
            child: Some(child),
            client,
            runtime,
            watcher: SupervisorWatcher {
                kind: WatcherKind::SpawnedChild,
                handle,
            },
        })
    }

    async fn reap_and_respawn_with_current_settings(self: &Arc<Self>) -> Result<()> {
        if !self
            .needs_respawn_on_next_thread_start
            .swap(false, Ordering::AcqRel)
        {
            return Ok(());
        }

        let result = self.reap_and_respawn_with_current_settings_inner().await;
        if result.is_err() {
            self.needs_respawn_on_next_thread_start
                .store(true, Ordering::Release);
        }
        result
    }

    async fn reap_and_respawn_with_current_settings_inner(self: &Arc<Self>) -> Result<()> {
        tracing::info!(
            target: "shared_codex_daemon::restart",
            "respawning shared codex app-server before thread/start because runtime settings changed"
        );
        self.transition_replace("settings changed", ReplacePrecondition::Always, true, None)
            .await?;
        Ok(())
    }

    /// Both arms go through `terminate_group_with_grace`: SIGTERM → exit-driven wait up to the
    /// stop grace → straggler cleanup.
    async fn reap_current_child_or_runtime(&self, running: Option<RunningProcessParts>) {
        let Some(RunningProcessParts {
            runtime,
            child,
            watcher,
        }) = running
        else {
            return;
        };
        watcher.handle.abort();
        if let Some(mut child) = child {
            terminate_group_with_grace(
                SignalScope::Group { pgid: runtime.pgid },
                ExitWait::Child(&mut child),
                self.stop_grace,
            )
            .await;
            return;
        }

        reap_verified_process_group(
            runtime.pid,
            runtime.pgid,
            runtime.process_start_time,
            &runtime.boot_id,
            self.stop_grace,
        )
        .await;
    }

    async fn remove_stale_socket_before_spawn(&self) -> Result<()> {
        if self.sock.exists() {
            reap_listener_if_alive(&self.sock, self.stop_grace).await?;
            let _ = std::fs::remove_file(&self.sock);
        }
        Ok(())
    }

    async fn persist_runtime_starting(
        &self,
        runtime: &SharedDaemonRuntime,
        last_error: Option<String>,
        daemon_env_signature: String,
    ) -> Result<()> {
        self.repo
            .shared_daemon_runtime_set(SharedCodexDaemonUpdate {
                state: SharedDaemonState::Starting.as_db_str().to_string(),
                pid: Some(runtime.pid),
                pgid: Some(runtime.pgid),
                sock_path: Some(self.sock.display().to_string()),
                codex_home_path: Some(self.home.path().display().to_string()),
                process_start_time: Some(runtime.process_start_time),
                boot_id: Some(runtime.boot_id.clone()),
                started_at: Some(runtime.started_at),
                last_error,
                increment_restart_count: false,
                daemon_env_signature: Some(daemon_env_signature),
            })
            .await?;
        // The DB row is no longer a Failed shape: the dedup cache must not
        // suppress a later Failed write against it.
        self.failed_persist_dedup.clear();
        Ok(())
    }

    /// Wait for the freshly spawned child to bind its socket and answer `initialize`. A missing
    /// socket is NOT a failure while the child is alive (codex may backfill for minutes); a dead
    /// child fails immediately; the deadline is a TOTAL cap that also cuts an in-flight attempt.
    async fn poll_connect_initialized(
        &self,
        spawn_guard: &mut SpawnedChildGuard,
    ) -> Result<(CodexAppServer, crate::codex_appserver::NotificationStream)> {
        let started = tokio::time::Instant::now();
        let deadline = started + self.start_timeout;
        loop {
            // The deadline caps the TOTAL wait, including an in-flight attempt, so a child that accepts
            // the socket but never answers cannot stretch it by the 10s per-attempt request timeout.
            let (deadline_hit_in_flight, attempt_err) =
                match tokio::time::timeout_at(deadline, connect_initialized(&self.sock)).await {
                    Ok(Ok(pair)) => return Ok(pair),
                    Ok(Err(e)) => (false, e.to_string()),
                    Err(_) => (
                        true,
                        "initialize attempt still in flight at the cold-start deadline".to_string(),
                    ),
                };
            // Probe liveness AFTER the (possibly long) attempt so the error describes the child's
            // state at emission time.
            if let Some(status) = spawn_guard.try_wait_exit() {
                return Err(CalmError::CodexAppServer(format!(
                    "shared codex app-server exited before initialize ({status}) \
                     after {:.1}s: {attempt_err}",
                    started.elapsed().as_secs_f64()
                )));
            }
            if deadline_hit_in_flight || tokio::time::Instant::now() >= deadline {
                let waited_ms = started.elapsed().as_millis() as u64;
                tracing::warn!(
                    target: "shared_codex_daemon::start",
                    waited_ms,
                    deadline_ms = self.start_timeout.as_millis() as u64,
                    error = %attempt_err,
                    "shared codex app-server missed the cold-start deadline; reaping spawn"
                );
                return Err(CalmError::CodexAppServer(format!(
                    "shared codex app-server not initialized after {:.1}s \
                     (deadline {}s, child still alive): {attempt_err}",
                    started.elapsed().as_secs_f64(),
                    self.start_timeout.as_secs()
                )));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Bounded readiness window for a verified `starting` child of a previous calm-server
    /// incarnation (no `Child` handle, so `verify_owned_pid` polling). `window` is the REMAINING
    /// budget; the deadline is a TOTAL cap that also cuts an in-flight attempt.
    async fn poll_adopt_initialized(
        &self,
        sock: &Path,
        pid: i32,
        start_time: u64,
        boot_id: &str,
        window: Duration,
    ) -> AdoptProbe {
        let started = tokio::time::Instant::now();
        let deadline = started + window;
        loop {
            let (deadline_hit_in_flight, attempt_err) =
                match tokio::time::timeout_at(deadline, connect_initialized(sock)).await {
                    Ok(Ok(pair)) => return AdoptProbe::Connected(pair),
                    Ok(Err(e)) => (false, e.to_string()),
                    Err(_) => (
                        true,
                        "initialize attempt still in flight at the readiness-window deadline"
                            .to_string(),
                    ),
                };
            // Liveness AFTER the (possibly long) attempt. Zombie = dead: an exited-but-unreaped child
            // can never bind the socket. This arm sends no signals, so members of a zombie leader's
            // group can leak past the fresh spawn.
            if !verify_owned_pid(pid, start_time, boot_id) || proc_pid_is_zombie(pid) {
                tracing::info!(
                    target: "shared_codex_daemon::start",
                    pid,
                    waited_ms = started.elapsed().as_millis() as u64,
                    "starting child exited during its readiness window; falling through to spawn"
                );
                return AdoptProbe::ChildExited;
            }
            if deadline_hit_in_flight || tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    target: "shared_codex_daemon::start",
                    pid,
                    waited_ms = started.elapsed().as_millis() as u64,
                    window_ms = window.as_millis() as u64,
                    error = %attempt_err,
                    "starting child missed its remaining readiness window"
                );
                return AdoptProbe::WindowLapsed {
                    last_error: attempt_err,
                };
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn running_client(&self) -> Option<Arc<CodexAppServer>> {
        let core = self.core.lock().await;
        match &core.state {
            SupervisorState::Running { client, .. } => Some(client.clone()),
            _ => None,
        }
    }

    /// Take an exited child out of the installed Running state, with the generation captured
    /// atomically under the core lock so the crash restart can pass `GenerationIs`.
    async fn try_take_exited_running_child(
        &self,
    ) -> Option<(std::process::ExitStatus, SharedDaemonRuntime, u64)> {
        let mut core = self.core.lock().await;
        let generation = core.generation;
        match &mut core.state {
            SupervisorState::Running { child, runtime, .. } => match child
                .as_mut()
                .and_then(|child| child.try_wait().ok())
                .flatten()
            {
                Some(status) => {
                    *child = None;
                    Some((status, runtime.clone(), generation))
                }
                None => None,
            },
            _ => None,
        }
    }

    async fn watch_spawned_child(this: std::sync::Weak<Self>) {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let Some(this) = this.upgrade() else {
                return;
            };
            let exited = this.try_take_exited_running_child().await;
            if let Some((status, runtime, generation)) = exited {
                let uptime_sec = (now_ms() - runtime.started_at).max(0) / 1000;
                let error = format!("shared codex app-server exited: {status}");
                tracing::warn!(
                    target = "shared_codex_daemon::stop",
                    uptime_sec,
                    exit_code = status.code(),
                    signal = status.signal(),
                    "shared codex app-server stopped"
                );
                // The restart runs as its OWN task: the replace transition aborts the captured watcher
                // handle (this task), so an inline restart would abort itself mid-transition.
                tokio::spawn(Arc::clone(&this).restart_after_crash(error, generation));
                // `restart_after_crash` spawns a new watcher; this loop must end here so we don't
                // accumulate one stale watcher per crash.
                return;
            }
        }
    }

    async fn watch_taken_over_pid(this: std::sync::Weak<Self>, runtime: SharedDaemonRuntime) {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if !verify_owned_pid(runtime.pid, runtime.process_start_time, &runtime.boot_id) {
                let Some(this) = this.upgrade() else {
                    return;
                };
                // Confirm the process we watched is still the installed Running incarnation and capture
                // its generation atomically; if another transition already replaced it, we are stale.
                let generation = {
                    let core = this.core.lock().await;
                    match &core.state {
                        SupervisorState::Running {
                            runtime: installed, ..
                        } if installed.pid == runtime.pid
                            && installed.process_start_time == runtime.process_start_time =>
                        {
                            Some(core.generation)
                        }
                        _ => None,
                    }
                };
                let Some(generation) = generation else {
                    return;
                };
                let uptime_sec = (now_ms() - runtime.started_at).max(0) / 1000;
                let error = format!(
                    "taken-over shared codex app-server exited: pid {}",
                    runtime.pid
                );
                tracing::warn!(
                    target = "shared_codex_daemon::stop",
                    pid = runtime.pid,
                    uptime_sec,
                    reason = "taken-over daemon exited",
                    "shared codex app-server takeover pid exited"
                );
                // Own task, not inline: the transition aborts this watcher's handle.
                tokio::spawn(Arc::clone(&this).restart_after_crash(error, generation));
                return;
            }
        }
    }

    /// Crash-watcher restart. The backoff sleep happens BEFORE acquiring the transition serial,
    /// and `GenerationIs` aborts silently if another transition already replaced the process.
    /// Boxed future: the watcher → restart → spawn → watcher cycle otherwise defeats `Send` inference.
    fn restart_after_crash(
        self: Arc<Self>,
        error: String,
        generation: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        Box::pin(async move {
            let count = self.restart_count.load(Ordering::SeqCst) + 1;
            tracing::warn!(
                target = "shared_codex_daemon::restart",
                prior_state = ?SharedDaemonState::Running,
                restart_count = count,
                last_error = %error,
                "restarting shared codex app-server"
            );
            let delay = self.restart_backoff.next_delay();
            tokio::time::sleep(delay).await;
            match self
                .transition_replace(
                    &error,
                    ReplacePrecondition::GenerationIs(generation),
                    true,
                    Some(error.clone()),
                )
                .await
            {
                Ok(ReplaceOutcome::Replaced) => {}
                Ok(ReplaceOutcome::PreconditionFailed) => {
                    tracing::info!(
                        target = "shared_codex_daemon::restart",
                        generation,
                        "crash restart aborted: another transition already replaced the crashed process"
                    );
                }
                // The observation-lost error is the one Err for which "terminalized + heal armed" would be
                // a false claim; recovery is the next transition or boot reconciliation.
                Err(e) if detached_spawn_result_lost(&e) => {
                    tracing::warn!(
                        target = "shared_codex_daemon::restart",
                        error = %e,
                        "crash restart outcome unobserved (detached spawn task panic/teardown); \
                         state unknown — recovery via the next transition or boot reconciliation"
                    );
                }
                Err(e) => {
                    // "recorded", not "persisted": the DB write only happens where a Failed row is trustworthy.
                    tracing::warn!(
                        target = "shared_codex_daemon::restart",
                        error = %e,
                        "crash restart failed; terminal failed state recorded and heal loop armed"
                    );
                }
            }
        })
    }

    /// Typestate transition: begin/finish a fresh process spawn. Caller MUST hold
    /// `transition_serial`. THE Failed-persistence choke point: any spawn failure persists a
    /// `failed` row with identity NULLed (absence is proven for both pre- and post-spawn cases).
    pub(crate) async fn start_new_process_typestate<F, Fut>(
        self: &Arc<Self>,
        spawn: F,
    ) -> Result<()>
    where
        F: FnOnce(PathBuf) -> Fut + Send,
        Fut: std::future::Future<Output = Result<LaunchedSharedDaemon>> + Send,
    {
        let socket_path = self.sock.clone();
        {
            let mut core = self.core.lock().await;
            core.state = SupervisorState::Starting {
                backoff_until: None,
                socket_path: socket_path.clone(),
            };
        }
        match spawn(socket_path).await {
            Ok(launched) => {
                let generation = {
                    let mut core = self.core.lock().await;
                    core.attempts = 0;
                    // A new Running incarnation is installed: bump the generation (wrapping; equality-only consumers).
                    core.generation = core.generation.wrapping_add(1);
                    core.state = SupervisorState::Running {
                        child: launched.child,
                        client: launched.client,
                        runtime: launched.runtime,
                        watcher: launched.watcher,
                    };
                    core.generation
                };
                // Readiness stamped on every installed Running.
                self.readiness.send_replace(DaemonReadiness {
                    generation,
                    running: true,
                });
                Ok(())
            }
            Err(err) => {
                let class = classify_spawn_failure(&err);
                let generation = {
                    let mut core = self.core.lock().await;
                    core.state = SupervisorState::Failed {
                        last_error: err.to_string(),
                        class,
                        since: Instant::now(),
                    };
                    core.generation
                };
                // Readiness stamped on every terminal Failed.
                self.readiness.send_replace(DaemonReadiness {
                    generation,
                    running: false,
                });
                // No DB I/O under the core mutex; the transition serial is
                // still held by the caller, so this write is ordered with
                // every other daemon-row write.
                self.persist_failed_deduped(
                    &err.to_string(),
                    class,
                    FailedIdentity::proven_absent(&self.sock, self.home.path()),
                )
                .await;
                self.schedule_heal();
                Err(err)
            }
        }
    }
}

impl SharedCodexAppServer {
    /// Arm the background heal loop: at most one instance via CAS on `heal_active`, released
    /// by [`HealActiveGuard`]'s RAII `Drop`. The task holds only a `Weak`.
    fn schedule_heal(self: &Arc<Self>) -> Option<JoinHandle<()>> {
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            return None;
        }
        if self
            .heal_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return None;
        }
        let guard = HealActiveGuard {
            flag: Arc::clone(&self.heal_active),
        };
        let weak = Arc::downgrade(self);
        Some(tokio::spawn(Self::heal_loop(weak, guard)))
    }

    /// Infinite classified retry — no give-up-after-N. Ok ⇒ release the claim, re-check the
    /// terminal state (re-arming on Failed), exit. Err ⇒ reclassify and continue.
    async fn heal_loop(this: std::sync::Weak<Self>, claim: HealActiveGuard) {
        let mut claim = Some(claim);
        loop {
            let Some(strong) = this.upgrade() else {
                return;
            };
            let delay = strong.next_heal_delay().await;
            let nudge = Arc::clone(&strong.heal_nudge);
            drop(strong);
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = nudge.notified() => {}
            }
            let Some(strong) = this.upgrade() else {
                return;
            };
            match strong.ensure_running().await {
                Ok(()) => {
                    // Fixtures gate: a test holding this mutex parks the healed task here.
                    #[cfg(feature = "fixtures")]
                    drop(strong.heal_post_ok_gate.lock().await);
                    // Release the singleton claim BEFORE the terminal re-check: `ensure_running()` Ok is not
                    // proof the daemon is still Running, and a concurrent failure's `schedule_heal()` CAS may
                    // have lost against the claim this task was still holding.
                    drop(claim.take());
                    let failed_behind_ok = matches!(
                        strong.core.lock().await.state,
                        SupervisorState::Failed { .. }
                    );
                    if failed_behind_ok {
                        strong.schedule_heal();
                    }
                    return;
                }
                Err(e) => {
                    // First occurrence / changes are warned by the persist
                    // path; the per-round line stays debug to avoid log
                    // amplification on a permanent cheap failure.
                    tracing::debug!(
                        target: "shared_codex_daemon::heal",
                        error = %e,
                        "heal round failed; retrying on the classified lane"
                    );
                }
            }
        }
    }

    /// Delay for the next heal round by lane, ±20% jitter; brief Running does NOT reset attempts.
    async fn next_heal_delay(&self) -> Duration {
        let class = {
            let core = self.core.lock().await;
            match &core.state {
                SupervisorState::Failed { class, .. } => *class,
                _ => FailureClass::Transient,
            }
        };
        let raw = match class {
            FailureClass::Transient => self.restart_backoff.next_delay(),
            FailureClass::Persistent | FailureClass::Unreconciled => self
                .restart_backoff
                .next_slow_delay(HEAL_SLOW_RETRY_CEILING),
        };
        heal_jitter(raw)
    }
}

/// Thread ids whose `thread_cache` attribution a committed delete dropped. The pending
/// registry binds FIFO without reading the thread id, so without a tombstone a deleted Card's
/// late `thread/started` would be handed to whatever unrelated Card is next in the queue.
#[derive(Default)]
struct ForgottenThreads {
    order: VecDeque<String>,
    set: HashSet<String>,
}

/// Cap for [`ForgottenThreads`]. ~4k short ids is well under a megabyte and far
/// more deleted-Card threads than can plausibly have a notification in flight.
const FORGOTTEN_THREAD_TOMBSTONE_CAP: usize = 4096;

impl ForgottenThreads {
    fn remember(&mut self, thread_id: &str) {
        if !self.set.insert(thread_id.to_string()) {
            return;
        }
        self.order.push_back(thread_id.to_string());
        while self.order.len() > FORGOTTEN_THREAD_TOMBSTONE_CAP {
            if let Some(evicted) = self.order.pop_front() {
                self.set.remove(&evicted);
            }
        }
    }

    fn contains(&self, thread_id: &str) -> bool {
        self.set.contains(thread_id)
    }
}

impl SharedCodexAppServer {
    async fn install_client(
        &self,
        client: Arc<CodexAppServer>,
        mut notifications: crate::codex_appserver::NotificationStream,
    ) -> Result<()> {
        if let Some(service) = &self.recovery {
            // New connection, new receiver; no job owns the replaceable client.
            service.install(&client)?;
        }
        // Stamp the daemon (re)connect wall-clock; common path for both fresh-spawn and hot-takeover.
        self.daemon_connected_at_ms
            .store(now_ms(), Ordering::SeqCst);
        let tx = self.notifications.clone();
        // The notification task must not keep its own client alive: a strong Arc here leaves the
        // old connection open forever. Upgrade only for the late-turn interrupt that needs an RPC.
        let client = Arc::downgrade(&client);
        let pending = self.pending_codex_threads_handle.clone();
        let repo = self.repo.clone();
        let thread_cache = self.thread_cache.clone();
        let active_turns = self.active_turns.clone();
        let sealed_turn_threads = self.sealed_turn_threads.clone();
        let kernel_initiated_threads = self.kernel_initiated_threads.clone();
        let forgotten_threads = self.forgotten_threads.clone();
        let kernel_thread_start_serial = self.kernel_thread_start_serial.clone();
        tokio::spawn(async move {
            while let Some(notification) = notifications.recv().await {
                if let Some(thread_id) = thread_started_id(&notification) {
                    match handle_thread_started_notification(
                        pending.as_ref(),
                        &repo,
                        &thread_cache,
                        &kernel_initiated_threads,
                        &forgotten_threads,
                        &kernel_thread_start_serial,
                        thread_id,
                    )
                    .await
                    {
                        Ok(ThreadStartedHandling::PendingBound) => continue,
                        Ok(ThreadStartedHandling::DispatchNormally) => {}
                        Err(e) => {
                            tracing::warn!(
                                target = "shared_codex_daemon::pending_bind",
                                %thread_id,
                                error = %e,
                                "failed to bind pending shared codex empty-card thread start"
                            );
                        }
                    }
                }
                if let Some((thread_id, turn_id)) =
                    track_active_turn(&active_turns, &sealed_turn_threads, &notification)
                {
                    let Some(client) = client.upgrade() else {
                        break;
                    };
                    let active_turns = active_turns.clone();
                    tokio::spawn(async move {
                        match client.turn_interrupt(&thread_id, &turn_id).await {
                            Ok(()) => {
                                active_turns.remove_if(&thread_id, |_, active| active == &turn_id);
                            }
                            Err(error) => {
                                // Keep the id in `active_turns`: deletion's
                                // strict quiesce can retry and must not mistake
                                // a failed best-effort interrupt for absence.
                                tracing::warn!(
                                    thread_id,
                                    turn_id,
                                    error = %error,
                                    "failed to interrupt late turn on deletion-sealed thread"
                                );
                            }
                        }
                    });
                }
                if let Some(thread_id) = turn_completed_thread_id(&notification) {
                    kernel_initiated_threads.lock().await.remove(thread_id);
                }
                let _ = tx.send(notification);
            }
        });
        Ok(())
    }

    async fn rebuild_thread_cache_from_db(&self) -> Result<()> {
        self.thread_cache.clear();
        self.active_turns.clear();

        let active_threads = merge_active_shared_thread_attribution(self.repo.as_ref()).await?;
        for (card_id, thread_id) in active_threads {
            if crate::isolated_codex::lookup::is_isolated_card(self.repo.as_ref(), &card_id).await?
            {
                continue;
            }
            self.thread_cache.insert(thread_id, card_id);
        }
        Ok(())
    }

    async fn resume_cached_threads(&self, mode: ResumeMode) {
        let Some(client) = self.running_client().await else {
            return;
        };
        for (thread_id, card_id) in self.resume_candidates() {
            // The candidate list is a SNAPSHOT; a delete can drop a mapping while this loop is parked,
            // so every candidate is re-validated HERE under `resume_replay_serial`, held across the
            // whole iteration including the RPC (releasing it first would leave the window).
            let _replay_guard = self.resume_replay_serial.lock().await;
            if self.cached_card_for_thread(&thread_id).as_deref() != Some(card_id.as_str()) {
                tracing::info!(
                    target = "shared_codex_daemon::resume",
                    %thread_id,
                    %card_id,
                    "skipping shared codex thread whose attribution was dropped \
                     while the resume replay was in flight"
                );
                continue;
            }
            tracing::info!(
                target = "shared_codex_daemon::resume",
                %thread_id,
                %card_id,
                "resuming shared codex thread"
            );
            if mode == ResumeMode::HotTakeover {
                Self::resume_thread_typed(&client, &thread_id, &card_id, ThreadConfig::NoMcp).await;
                continue;
            }

            let (role, raw_token) = match write_in_tx_typed(self.repo.as_ref(), {
                let thread_id = thread_id.clone();
                let card_id = card_id.clone();
                move |tx| {
                    Box::pin(async move {
                        let shape=crate::db::sqlite::card_execution_shape_tx(tx,&card_id).await?;
                        let Some(runtime) =
                            session_projection_active_for_card_tx(tx, &card_id).await?
                        else {
                            // Decide inside the same transaction as the token
                            // choice: a systemError can arrive during replay.
                            let failed: bool = sqlx::query_scalar(
                                "SELECT EXISTS(SELECT 1 FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id WHERE c.id=?1 \
                                    AND ws.state='failed' AND json_extract(ws.handle_state_json,'$.mode')=?2)"
                            ).bind(&card_id).bind(calm_types::harness::HARNESS_MODE).fetch_one(&mut **tx).await?;
                            return Ok(if failed { ColdResumeAuthorization::Skip } else { ColdResumeAuthorization::NoMcp });
                        };
                        if runtime.thread_id.as_deref() != Some(thread_id.as_str()) {
                            return Ok(ColdResumeAuthorization::NoMcp);
                        }
                        // One decoder for harness identity: a Planner card without a known Codex binding is not a
                        // harness card and gets no credential; a conversation profile without an MCP role neither.
                        let binding = crate::harness::profile::PlannerBinding::from_shape(&shape.kind,shape.role,&shape.payload);
                        let no_mcp = match &binding {
                            Some(binding) => binding.profile.mcp_role().is_none()
                                || binding.provider != crate::session_projection_repo::AgentProvider::Codex,
                            None => shape.role == CardRole::Planner,
                        };
                        if no_mcp {
                            return Ok(ColdResumeAuthorization::NoMcp);
                        }
                        let raw=mint_and_persist_card_token(tx, &card_id, &runtime.id).await?;
                        Ok(ColdResumeAuthorization::Token {role:shape.role,raw})
                    })
                }
            })
            .await
            {
                Ok(ColdResumeAuthorization::Token {role,raw}) => (role,raw),
                Ok(ColdResumeAuthorization::Skip) => continue,
                Ok(ColdResumeAuthorization::NoMcp) => {
                    Self::resume_thread_typed(&client, &thread_id, &card_id, ThreadConfig::NoMcp)
                        .await;
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        target = "shared_codex_daemon::resume",
                        %thread_id,
                        %card_id,
                        error = %e,
                        "shared codex thread token refresh failed; leaving the thread unloaded"
                    );
                    continue;
                }
            };
            // Only the cold respawn caller may rotate and reemit per-card MCP config, and only for the
            // card's active thread; hot takeover plain-resumes because loaded threads ignore resume config.
            Self::resume_thread_typed(
                &client,
                &thread_id,
                &card_id,
                ThreadConfig::McpShell {
                    role,
                    socket_path: self.kernel_mcp_socket_path.clone(),
                    raw_token,
                },
            )
            .await;
        }
    }

    async fn resume_thread_typed(
        client: &CodexAppServer,
        thread_id: &str,
        card_id: &str,
        config: ThreadConfig,
    ) {
        let lowered = match config.to_wire_config() {
            Ok(lowered) => lowered,
            Err(e) => {
                tracing::warn!(
                    target = "shared_codex_daemon::resume",
                    %thread_id,
                    %card_id,
                    error = %e,
                    "shared codex thread resume config unavailable; leaving mapping intact"
                );
                return;
            }
        };
        if let Err(e) = client.thread_resume_with_config(thread_id, lowered).await {
            tracing::warn!(
                target = "shared_codex_daemon::resume",
                %thread_id,
                %card_id,
                error = %e,
                "shared codex thread resume failed; leaving mapping intact"
            );
        }
    }
}

#[cfg(any(test, feature = "fixtures"))]
impl SharedCodexAppServer {
    /// Hold the production serialization primitive shared by `handle_thread_started_notification`
    /// and [`Self::forget_threads_for_deleted_cards`].
    #[cfg(feature = "fixtures")]
    pub async fn lock_thread_start_serial_for_test(&self) -> tokio::sync::OwnedMutexGuard<()> {
        Arc::clone(&self.kernel_thread_start_serial)
            .lock_owned()
            .await
    }

    /// What a reconnect would CONSIDER resuming, sorted for a stable assertion.
    #[cfg(feature = "fixtures")]
    pub fn resume_candidates_for_test(&self) -> Vec<(String, String)> {
        let mut candidates = self.resume_candidates();
        candidates.sort();
        candidates
    }

    #[cfg(feature = "fixtures")]
    pub fn active_turn_for_test(&self, thread_id: &str) -> Option<String> {
        self.active_turns
            .get(thread_id)
            .map(|entry| entry.value().clone())
    }

    #[cfg(feature = "fixtures")]
    pub fn turn_start_count_for_test(&self) -> u64 {
        self.fake
            .as_ref()
            .map(|fake| fake.next_turn.load(Ordering::SeqCst).saturating_sub(1))
            .unwrap_or(0)
    }

    #[cfg(feature = "fixtures")]
    pub fn started_turns_for_test(&self) -> Vec<(String, Vec<InputItem>)> {
        self.fake
            .as_ref()
            .map(|fake| {
                fake.started_turns
                    .lock()
                    .expect("fake shared codex started turns mutex poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }

    /// What each `turn/start` asked of the model; pairs index-for-index with `started_turns_for_test`.
    #[cfg(feature = "fixtures")]
    pub fn started_turn_selections_for_test(&self) -> Vec<(String, TurnModelSelection)> {
        self.fake
            .as_ref()
            .map(|fake| {
                fake.started_turn_selections
                    .lock()
                    .expect("fake shared codex turn selections mutex poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }

    /// The `clientUserMessageId` each `turn/start` carried; pairs index-for-index with
    /// `started_turns_for_test`.
    #[cfg(feature = "fixtures")]
    pub fn started_turn_client_ids_for_test(&self) -> Vec<Option<String>> {
        self.fake
            .as_ref()
            .map(|fake| {
                fake.started_turn_client_ids
                    .lock()
                    .expect("fake shared codex turn client ids mutex poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }

    #[cfg(feature = "fixtures")]
    pub fn started_thread_params_for_test(&self) -> Vec<StartedThreadParam> {
        self.fake
            .as_ref()
            .map(|fake| {
                fake.started_thread_params
                    .lock()
                    .expect("fake shared codex thread params mutex poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }

    #[cfg(feature = "fixtures")]
    pub fn interrupted_turns_for_test(&self) -> Vec<(String, String)> {
        self.fake
            .as_ref()
            .map(|fake| {
                fake.interrupted_turns
                    .lock()
                    .expect("fake shared codex interrupted turns mutex poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }

    #[cfg(feature = "fixtures")]
    pub fn install_turn_start_return_hook_for_test(&self, hook: TurnStartReturnHook) {
        if let Some(fake) = self.fake.as_ref() {
            *fake
                .turn_start_return_hook
                .lock()
                .expect("fake shared codex turn-start hook mutex poisoned") = Some(hook);
        }
    }

    /// Every `turn/steer` the fake was handed, in order.
    #[cfg(feature = "fixtures")]
    pub fn steered_turns_for_test(&self) -> Vec<SteeredTurnParam> {
        self.fake
            .as_ref()
            .map(|fake| {
                fake.steered_turns
                    .lock()
                    .expect("fake shared codex steered turns mutex poisoned")
                    .clone()
            })
            .unwrap_or_default()
    }

    /// Make every subsequent `turn/steer` be REFUSED by codex with `message`
    /// — an answer, not an outage — or accepted again with `None`.
    #[cfg(feature = "fixtures")]
    pub fn reject_turn_steer_for_test(&self, message: Option<&str>) {
        if let Some(fake) = self.fake.as_ref() {
            *fake
                .reject_turn_steer
                .lock()
                .expect("fake shared codex reject-steer mutex poisoned") =
                message.map(ToOwned::to_owned);
        }
    }

    /// Make every subsequent `turn/steer` go UNANSWERED (the client's own timeout error) or
    /// answered again with `false`.
    #[cfg(feature = "fixtures")]
    pub fn fail_turn_steer_for_test(&self, fail: bool) {
        if let Some(fake) = self.fake.as_ref() {
            fake.fail_turn_steer.store(fail, Ordering::SeqCst);
        }
    }

    /// Hold the next `turn/steer` inside the daemon, after it is recorded
    /// and before it is answered, until the test releases it.
    #[cfg(feature = "fixtures")]
    pub fn install_turn_steer_return_hook_for_test(&self, hook: TurnStartReturnHook) {
        if let Some(fake) = self.fake.as_ref() {
            *fake
                .turn_steer_return_hook
                .lock()
                .expect("fake shared codex turn-steer hook mutex poisoned") = Some(hook);
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn turn_thread_is_sealed_for_test(&self, thread_id: &str) -> bool {
        self.turn_thread_is_sealed(thread_id)
    }

    /// Answer `config/read` with this instead of failing.
    #[cfg(feature = "fixtures")]
    pub fn set_config_read_for_test(&self, config: CodexConfig) {
        if let Some(fake) = self.fake.as_ref() {
            *fake
                .config_read
                .lock()
                .expect("fake shared codex config-read mutex poisoned") = Some(config);
        }
    }

    /// Make every subsequent `model/list` be REFUSED by codex.
    #[cfg(feature = "fixtures")]
    pub fn reject_model_list_for_test(&self) {
        if let Some(fake) = self.fake.as_ref() {
            fake.reject_model_list.store(true, Ordering::SeqCst);
        }
    }

    /// Make every subsequent `config/read` be REFUSED by codex.
    #[cfg(feature = "fixtures")]
    pub fn reject_config_read_for_test(&self) {
        if let Some(fake) = self.fake.as_ref() {
            fake.reject_config_read.store(true, Ordering::SeqCst);
        }
    }

    /// Make every subsequent `turn/start` be REFUSED by codex — an answer,
    /// not an outage.
    #[cfg(feature = "fixtures")]
    pub fn reject_turn_start_for_test(&self) {
        if let Some(fake) = self.fake.as_ref() {
            fake.reject_turn_start.store(true, Ordering::SeqCst);
        }
    }

    /// Let `turn/start` succeed again, as codex does when it comes back.
    #[cfg(feature = "fixtures")]
    pub fn clear_turn_start_failure_for_test(&self) {
        if let Some(fake) = self.fake.as_ref() {
            fake.fail_turn_start.store(false, Ordering::SeqCst);
            fake.reject_turn_start.store(false, Ordering::SeqCst);
        }
    }

    /// Make every subsequent `turn/start` fail, as codex does when it refuses
    /// a turn outright.
    #[cfg(feature = "fixtures")]
    pub fn fail_turn_start_for_test(&self) {
        if let Some(fake) = self.fake.as_ref() {
            fake.fail_turn_start.store(true, Ordering::SeqCst);
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn fail_next_thread_start_for_test(&self) {
        if let Some(fake) = self.fake.as_ref() {
            fake.fail_next_thread_start.store(true, Ordering::SeqCst);
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn fail_turn_interrupt_for_test(&self, fail: bool) {
        if let Some(fake) = self.fake.as_ref() {
            fake.fail_turn_interrupt.store(fail, Ordering::SeqCst);
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn notification_receiver_count_for_test(&self) -> usize {
        self.notifications.receiver_count()
    }

    #[cfg(feature = "fixtures")]
    pub fn emit_turn_started_for_test(&self, thread_id: &str, turn_id: &str) {
        let _ = self.notifications.send(Notification::TurnStarted {
            thread_id: thread_id.to_string(),
            turn: serde_json::json!({ "id": turn_id }),
        });
    }

    #[cfg(feature = "fixtures")]
    pub fn emit_notification_for_test(&self, notification: Notification) {
        let _ = self.notifications.send(notification);
    }

    #[cfg(feature = "fixtures")]
    pub fn set_active_turn_for_test(&self, thread_id: &str, turn_id: &str) {
        self.active_turns
            .insert(thread_id.to_string(), turn_id.to_string());
    }

    #[cfg(feature = "fixtures")]
    pub async fn mark_kernel_initiated_thread_for_test(&self, thread_id: &str) {
        self.kernel_initiated_threads
            .lock()
            .await
            .insert(thread_id.to_string());
    }

    #[cfg(feature = "fixtures")]
    pub async fn handle_thread_started_notification_for_test(
        &self,
        thread_id: &str,
    ) -> Result<bool> {
        let handled = handle_thread_started_notification(
            self.pending_codex_threads_handle.as_ref(),
            &self.repo,
            &self.thread_cache,
            &self.kernel_initiated_threads,
            &self.forgotten_threads,
            &self.kernel_thread_start_serial,
            thread_id,
        )
        .await?;
        Ok(matches!(handled, ThreadStartedHandling::PendingBound))
    }

    pub fn sock_path(&self) -> &Path {
        &self.sock
    }

    pub async fn spawn_env_for_test(
        &self,
    ) -> Result<std::collections::BTreeMap<String, Option<String>>> {
        let snapshot = self.load_spawn_env_snapshot().await?;
        let mut cmd = Command::new(&self.codex_bin);
        self.apply_spawn_env(&mut cmd, &snapshot);
        Ok(cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect())
    }

    pub fn needs_respawn_on_next_thread_start_for_test(&self) -> bool {
        self.needs_respawn_on_next_thread_start
            .load(Ordering::SeqCst)
    }

    /// Whether the singleton heal-loop claim is currently held.
    pub fn heal_active_for_test(&self) -> bool {
        self.heal_active.load(Ordering::SeqCst)
    }

    /// Arm the heal loop directly (same CAS/RAII path as production).
    pub fn schedule_heal_for_test(self: &Arc<Self>) -> Option<JoinHandle<()>> {
        self.schedule_heal()
    }

    /// The heal loop's post-Ok interleaving gate: locking it parks the heal task AFTER a
    /// successful round but BEFORE the singleton claim is released.
    #[cfg(feature = "fixtures")]
    pub fn heal_post_ok_gate_for_test(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.heal_post_ok_gate)
    }

    /// The transition-entry interleaving gate: locking it parks a `transition_replace` AFTER
    /// the state left Running but BEFORE the reap/start body.
    #[cfg(feature = "fixtures")]
    pub fn transition_entry_gate_for_test(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.transition_entry_gate)
    }

    /// Current Running-incarnation generation.
    pub async fn generation_for_test(&self) -> u64 {
        self.core.lock().await.generation
    }

    /// Abort the most recent DETACHED spawn transition task; returns whether a handle was present.
    #[cfg(feature = "fixtures")]
    pub fn abort_detached_spawn_transition_for_test(&self) -> bool {
        let handle = self
            .detached_spawn_task
            .lock()
            .expect("detached spawn task seam mutex poisoned")
            .take();
        match handle {
            Some(handle) => {
                handle.abort();
                true
            }
            None => false,
        }
    }

    /// Acquire the transition serial as the owned by-value token the `_locked` chain threads.
    #[cfg(feature = "fixtures")]
    pub async fn lock_transition_serial_for_test(&self) -> tokio::sync::OwnedMutexGuard<()> {
        Arc::clone(&self.transition_serial).lock_owned().await
    }

    /// The raw detached spawn transition as an un-polled future, so a test can poll it once
    /// and then DROP it.
    #[cfg(feature = "fixtures")]
    pub fn detached_spawn_transition_future_for_test(
        self: &Arc<Self>,
        serial: tokio::sync::OwnedMutexGuard<()>,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static {
        let this = Arc::clone(self);
        async move { this.spawn_process_transition(serial, false, None).await }
    }

    /// Drive the atomic replace transition with an explicit precondition.
    pub async fn transition_replace_for_test(
        self: &Arc<Self>,
        reason: &str,
        pre: ReplacePrecondition,
    ) -> Result<ReplaceOutcome> {
        self.transition_replace(reason, pre, true, Some(reason.to_string()))
            .await
    }

    pub async fn taken_over_pid_watcher_active_for_test(&self) -> bool {
        let core = self.core.lock().await;
        matches!(
            &core.state,
            SupervisorState::Running {
                watcher: SupervisorWatcher {
                    kind: WatcherKind::TakenOverPid { .. },
                    ..
                },
                ..
            }
        )
    }
}

/// Where the termination signals go.
enum SignalScope {
    /// Signal the whole process group (the normal spawn-invariant target).
    Group { pgid: i32 },
    /// Signal a bare pid — ONLY for `reap_listener_if_alive`'s
    /// getpgid-failure fallback, where no valid pgid is derivable.
    #[cfg(target_os = "linux")]
    Pid { pid: i32 },
}

impl SignalScope {
    fn send(&self, signal: i32) {
        match *self {
            SignalScope::Group { pgid } => {
                signal_process_group(pgid, signal);
            }
            #[cfg(target_os = "linux")]
            SignalScope::Pid { pid } => {
                // SAFETY: kill(2) on a pid this supervisor verified/derived.
                unsafe {
                    libc::kill(pid, signal);
                }
            }
        }
    }

    fn describe(&self) -> (&'static str, i32) {
        match *self {
            SignalScope::Group { pgid } => ("pgid", pgid),
            #[cfg(target_os = "linux")]
            SignalScope::Pid { pid } => ("pid", pid),
        }
    }
}

/// How the helper observes the LEADER's exit.
enum ExitWait<'a> {
    /// We own the `Child` handle: exit-driven `child.wait()`.
    Child(&'a mut Child),
    /// Identity-verified external process: poll `verify_owned_pid` (a
    /// post-signal zombie counts as exited).
    VerifiedIdentity {
        pid: i32,
        start_time: u64,
        boot_id: &'a str,
    },
    /// `reap_listener_if_alive` targets: `/proc` presence poll (a
    /// post-signal zombie counts as exited).
    #[cfg(target_os = "linux")]
    ProcPresence { pid: i32 },
}

/// How the leader's exit was observed, which decides what straggler cleanup is safe: once
/// a group's last member is reaped the kernel may recycle the numeric pgid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaderExit {
    /// Exit observed with the leader still an UNREAPED zombie. Only an owned `Child` guarantees
    /// the pin (WE defer `wait()`); a non-owned zombie can be reaped by its parent at any moment,
    /// so it is treated like `FullyGone`.
    ExitedPinned,
    /// The leader was fully reaped: the group may be empty and its id recycled — no group-wide
    /// signal is ever sent; remaining members are swept individually.
    FullyGone,
    /// Grace ceiling elapsed with the leader still alive (not a zombie):
    /// the live leader is a member, so the group provably exists and
    /// cannot be recycled — group SIGKILL as before.
    AliveAtDeadline,
}

/// THE shared exit-driven termination helper: SIGTERM → wait for the LEADER's exit up to
/// `grace` (a ceiling, not a sleep) → straggler cleanup: group SIGKILL only when the group is
/// provably pinned (live leader or owned unreaped child), else a per-member verify-then-signal sweep.
async fn terminate_group_with_grace(
    scope: SignalScope,
    wait: ExitWait<'_>,
    grace: Duration,
) -> LeaderExit {
    let (scope_kind, scope_id) = scope.describe();
    // An owned child ALREADY reaped before this helper ran means the numeric pgid may be
    // recycled: nothing here is safely signalable anymore, not even the initial SIGTERM.
    if let ExitWait::Child(child) = &wait
        && child.id().is_none()
    {
        tracing::debug!(
            target: "shared_codex_daemon::stop",
            scope_kind,
            scope_id,
            "owned child already reaped before termination; skipping group signals (pgid may be recycled)"
        );
        return LeaderExit::FullyGone;
    }
    scope.send(libc::SIGTERM);
    let deadline = tokio::time::Instant::now() + grace;
    let (outcome, waited_child) = match wait {
        ExitWait::Child(child) => {
            // Observe WITHOUT reaping: `child.wait()` would release the zombie and un-pin the pgid
            // before the final group signal.
            let outcome = match child.id().and_then(|raw| i32::try_from(raw).ok()) {
                Some(pid) => {
                    poll_leader_exit(deadline, || {
                        proc_pid_is_zombie(pid).then_some(LeaderExit::ExitedPinned)
                    })
                    .await
                }
                // Unreachable after the pre-check above; defensive.
                None => LeaderExit::FullyGone,
            };
            (outcome, Some(child))
        }
        ExitWait::VerifiedIdentity {
            pid,
            start_time,
            boot_id,
        } => (
            // Post-signal poll: zombie = dead.
            poll_leader_exit(deadline, || {
                if !verify_owned_pid(pid, start_time, boot_id) {
                    Some(LeaderExit::FullyGone)
                } else if proc_pid_is_zombie(pid) {
                    Some(LeaderExit::ExitedPinned)
                } else {
                    None
                }
            })
            .await,
            None,
        ),
        #[cfg(target_os = "linux")]
        ExitWait::ProcPresence { pid } => (
            poll_leader_exit(deadline, || {
                if !proc_pid_present(pid) {
                    Some(LeaderExit::FullyGone)
                } else if proc_pid_is_zombie(pid) {
                    Some(LeaderExit::ExitedPinned)
                } else {
                    None
                }
            })
            .await,
            None,
        ),
    };
    let owned_child = waited_child.is_some();
    match outcome {
        LeaderExit::ExitedPinned if owned_child => {
            // Owned Child: WE hold the unreaped zombie, so the pgid is provably pinned and the group
            // SIGKILL is safe.
            tracing::debug!(
                target: "shared_codex_daemon::stop",
                scope_kind,
                scope_id,
                "owned child exited within the stop grace after SIGTERM; held zombie pins the group — final SIGKILL for stragglers"
            );
            scope.send(libc::SIGKILL);
        }
        LeaderExit::AliveAtDeadline => {
            tracing::warn!(
                target: "shared_codex_daemon::stop",
                scope_kind,
                scope_id,
                grace_ms = grace.as_millis() as u64,
                "stop-grace ceiling elapsed without a verified exit; escalating to SIGKILL"
            );
            scope.send(libc::SIGKILL);
        }
        LeaderExit::FullyGone if owned_child => {
            // Defensive-only arm (the pre-signal early return above
            // handles the already-reaped owned child): keep the r1 skip.
            tracing::debug!(
                target: "shared_codex_daemon::stop",
                scope_kind,
                scope_id,
                "owned child already reaped; skipping final SIGKILL"
            );
        }
        LeaderExit::ExitedPinned | LeaderExit::FullyGone => {
            // Non-owned leader dead: the external parent may reap the zombie between observation and
            // a group signal, and the kernel may recycle the pgid, so NO group-wide signal is sent.
            match scope {
                SignalScope::Group { pgid } => {
                    let sweep = sigkill_verified_group_members(pgid);
                    tracing::debug!(
                        target: "shared_codex_daemon::stop",
                        scope_kind,
                        scope_id,
                        outcome = ?outcome,
                        killed = ?sweep.killed,
                        skipped_zombies = ?sweep.skipped_zombies,
                        verify_failed = ?sweep.verify_failed,
                        "non-owned leader dead (zombie or reaped); swept remaining group members individually instead of group SIGKILL"
                    );
                }
                #[cfg(target_os = "linux")]
                SignalScope::Pid { .. } => {
                    // Pid-fallback scope: there is no pgid to enumerate and a bare-pid SIGKILL races recycling.
                    tracing::debug!(
                        target: "shared_codex_daemon::stop",
                        scope_kind,
                        scope_id,
                        outcome = ?outcome,
                        "non-owned pid-scope leader dead; nothing further safely signalable"
                    );
                }
            }
        }
    }
    // Reap / settle so callers' post-checks observe the result. The owned
    // child is reaped only NOW — after the final signal decision — so its
    // zombie pinned the group identity for the SIGKILL above.
    match waited_child {
        Some(child) => {
            let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
        }
        None if outcome != LeaderExit::FullyGone => {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        None => {}
    }
    outcome
}

/// ≤100ms observation poll against the grace deadline; `AliveAtDeadline` when the ceiling elapses.
async fn poll_leader_exit(
    deadline: tokio::time::Instant,
    mut observe: impl FnMut() -> Option<LeaderExit>,
) -> LeaderExit {
    loop {
        if let Some(outcome) = observe() {
            return outcome;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return LeaderExit::AliveAtDeadline;
        }
        tokio::time::sleep(Duration::from_millis(100).min(deadline - now)).await;
    }
}

/// SIGTERM → exit-driven wait up to `grace` → straggler cleanup (this path does not own the
/// leader's zombie, so a dead leader triggers the per-member sweep).
async fn reap_verified_process_group(
    pid: i32,
    pgid: i32,
    start_time: u64,
    boot_id: &str,
    grace: Duration,
) {
    terminate_group_with_grace(
        SignalScope::Group { pgid },
        ExitWait::VerifiedIdentity {
            pid,
            start_time,
            boot_id,
        },
        grace,
    )
    .await;

    // Post-signal check: zombie = dead (the leader may linger until its parent waits).
    if survivor_alive_after_group_reap(pid, start_time, boot_id) {
        tracing::warn!(
            target: "shared_codex_daemon::stop",
            pid,
            pgid,
            "after SIGKILL pgid, original launcher pid still verified; unexpected"
        );
    }
}

fn thread_started_id(notification: &Notification) -> Option<&str> {
    match notification {
        Notification::ThreadStarted { params } => thread_id_from_started(params),
        _ => None,
    }
}

pub fn thread_id_from_started(params: &serde_json::Value) -> Option<&str> {
    if let Some(id) = params
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(serde_json::Value::as_str)
    {
        return Some(id);
    }
    params.get("threadId").and_then(serde_json::Value::as_str)
}

fn turn_completed_thread_id(notification: &Notification) -> Option<&str> {
    match notification {
        Notification::TurnCompleted { thread_id, .. } => Some(thread_id),
        _ => None,
    }
}

fn turn_id(turn: &serde_json::Value) -> Option<&str> {
    turn.get("id").and_then(serde_json::Value::as_str)
}

fn other_turn_id(params: &serde_json::Value) -> Option<&str> {
    params
        .get("turn")
        .and_then(turn_id)
        .or_else(|| params.get("turnId").and_then(serde_json::Value::as_str))
}

pub fn other_thread_id(params: &serde_json::Value) -> Option<&str> {
    params.get("threadId").and_then(serde_json::Value::as_str)
}

fn track_active_turn(
    active_turns: &DashMap<String, String>,
    sealed_turn_threads: &DashMap<String, ()>,
    notification: &Notification,
) -> Option<(String, String)> {
    match notification {
        Notification::TurnStarted { thread_id, turn } => {
            if let Some(turn_id) = turn_id(turn) {
                if sealed_turn_threads.contains_key(thread_id) {
                    active_turns.insert(thread_id.clone(), turn_id.to_string());
                    return Some((thread_id.clone(), turn_id.to_string()));
                }
                active_turns.insert(thread_id.clone(), turn_id.to_string());
                if sealed_turn_threads.contains_key(thread_id) {
                    return Some((thread_id.clone(), turn_id.to_string()));
                }
            }
        }
        Notification::TurnCompleted { thread_id, turn } => {
            if let Some(turn_id) = turn_id(turn) {
                active_turns.remove_if(thread_id, |_, active| active == turn_id);
            } else {
                active_turns.remove(thread_id);
            }
        }
        Notification::Other { method, params } if method == "turn/aborted" => {
            if let Some(thread_id) = other_thread_id(params) {
                if let Some(turn_id) = other_turn_id(params) {
                    active_turns.remove_if(thread_id, |_, active| active == turn_id);
                } else {
                    active_turns.remove(thread_id);
                }
            }
        }
        _ => {}
    }
    None
}

enum ThreadStartedHandling {
    PendingBound,
    DispatchNormally,
}

async fn handle_thread_started_notification(
    pending: Option<&Arc<PendingThreadStartRegistry>>,
    repo: &Arc<dyn Repo>,
    thread_cache: &Arc<DashMap<String, String>>,
    kernel_initiated_threads: &Arc<Mutex<HashSet<String>>>,
    forgotten_threads: &Arc<Mutex<ForgottenThreads>>,
    kernel_thread_start_serial: &Arc<Mutex<()>>,
    thread_id: &str,
) -> Result<ThreadStartedHandling> {
    let _start_guard = kernel_thread_start_serial.lock().await;
    if kernel_initiated_threads.lock().await.contains(thread_id) {
        tracing::debug!(
            target: "shared_codex_daemon::pending_skip_kernel_initiated",
            %thread_id,
            "shared codex thread/started belongs to a kernel-initiated thread"
        );
        return Ok(ThreadStartedHandling::DispatchNormally);
    }

    let Some(pending) = pending else {
        return Ok(ThreadStartedHandling::DispatchNormally);
    };
    let already_mapped = if thread_cache.contains_key(thread_id) {
        true
    } else if let Some(card_id) =
        resolve_card_for_thread(repo.as_ref(), AgentProvider::Codex, thread_id).await?
    {
        thread_cache.insert(thread_id.to_string(), card_id);
        true
    } else {
        false
    };
    if already_mapped {
        tracing::debug!(
            target: "shared_codex_daemon::pending_skip_already_mapped",
            %thread_id,
            "shared codex thread/started already has a card mapping"
        );
        return Ok(ThreadStartedHandling::DispatchNormally);
    }

    // A thread a committed delete forgot is NOT an ownerless thread the FIFO registry may
    // hand to the next pending Card; the correct owner is nobody.
    if forgotten_threads.lock().await.contains(thread_id) {
        tracing::warn!(
            target: "shared_codex_daemon::pending_skip_forgotten_thread",
            %thread_id,
            "shared codex thread/started belongs to a deleted card's forgotten thread; not binding it to a pending card"
        );
        return Ok(ThreadStartedHandling::DispatchNormally);
    }

    match pending.on_thread_started(thread_id).await? {
        Some(card_id) => {
            thread_cache.insert(thread_id.to_string(), card_id);
            Ok(ThreadStartedHandling::PendingBound)
        }
        None => Ok(ThreadStartedHandling::DispatchNormally),
    }
}

#[cfg(target_os = "linux")]
async fn reap_listener_if_alive(sock_path: &Path, grace: Duration) -> Result<()> {
    let Ok(stream) = UnixStream::connect(sock_path).await else {
        return Ok(());
    };

    let peer_pid = match stream.peer_cred().and_then(|cred| {
        cred.pid().filter(|pid| *pid > 0).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "listener peer PID is unavailable",
            )
        })
    }) {
        Ok(pid) => pid,
        Err(error) => {
            tracing::warn!(
                target: "shared_codex_daemon::stop",
                %error,
                sock = %sock_path.display(),
                "SO_PEERCRED failed; proceeding to unlink listener-bound socket without reap"
            );
            return Ok(());
        }
    };
    let pgid = unsafe { libc::getpgid(peer_pid) };
    if pgid < 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!(
            target: "shared_codex_daemon::stop",
            peer_pid,
            error = %err,
            sock = %sock_path.display(),
            "getpgid failed; falling back to pid-only reap of stale socket listener"
        );
        drop(stream);
        // Graceful pid-fallback reap (exit observed via /proc).
        terminate_group_with_grace(
            SignalScope::Pid { pid: peer_pid },
            ExitWait::ProcPresence { pid: peer_pid },
            grace,
        )
        .await;
        return Ok(());
    }

    tracing::warn!(
        target: "shared_codex_daemon::stop",
        peer_pid,
        pgid,
        sock = %sock_path.display(),
        "stale socket has live listener; reaping orphaned daemon pgid before unlink"
    );
    drop(stream);
    // Graceful group reap: SIGTERM → exit-driven wait → straggler cleanup.
    terminate_group_with_grace(
        SignalScope::Group { pgid },
        ExitWait::ProcPresence { pid: peer_pid },
        grace,
    )
    .await;
    Ok(())
}

#[cfg(target_os = "macos")]
async fn reap_listener_if_alive(sock_path: &Path, grace: Duration) -> Result<()> {
    let Ok(stream) = UnixStream::connect(sock_path).await else {
        return Ok(());
    };
    let peer_pid = stream
        .peer_cred()
        .map_err(|error| CalmError::CodexAppServer(format!("read listener credentials: {error}")))?
        .pid()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| CalmError::CodexAppServer("listener peer PID is unavailable".into()))?;
    let observed_pgid = unsafe { libc::getpgid(peer_pid) };
    let pgid = if observed_pgid > 1 && observed_pgid == peer_pid {
        Some(observed_pgid)
    } else {
        if observed_pgid < 0 {
            let error = std::io::Error::last_os_error();
            tracing::warn!(
                target: "shared_codex_daemon::stop",
                peer_pid,
                %error,
                sock = %sock_path.display(),
                "getpgid failed; falling back to pid-only reap of stale socket listener"
            );
        } else {
            tracing::warn!(
                target: "shared_codex_daemon::stop",
                peer_pid,
                observed_pgid,
                sock = %sock_path.display(),
                "listener process group does not match its pid; falling back to pid-only reap"
            );
        }
        None
    };
    // Register the exit observation right after the peer connection identified the listener:
    // an open connection does not pin the peer pid, so registering here narrows the recycle window.
    let watcher = macos_process::ExitWatcher::new(peer_pid).map_err(|error| {
        CalmError::CodexAppServer(format!("watch listener process {peer_pid}: {error}"))
    })?;
    drop(stream);
    macos_process::terminate_listener(watcher, pgid, grace)
        .await
        .map_err(|error| {
            CalmError::CodexAppServer(format!("reap listener process {peer_pid}: {error}"))
        })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn reap_listener_if_alive(_sock_path: &Path, _grace: Duration) -> Result<()> {
    Err(CalmError::CodexAppServer(
        "stale listener reaping requires Linux or macOS".into(),
    ))
}

struct SpawnedChildGuard {
    spawned_child: Option<Child>,
    pgid: i32,
}

impl SpawnedChildGuard {
    fn new(child: Child, pgid: i32) -> Self {
        Self {
            spawned_child: Some(child),
            pgid,
        }
    }

    fn disarm(&mut self) -> Child {
        self.spawned_child
            .take()
            .expect("spawn guard disarmed once")
    }

    /// Non-blocking liveness probe on the still-guarded child; `None` while it runs or on a probe error.
    fn try_wait_exit(&mut self) -> Option<std::process::ExitStatus> {
        self.spawned_child
            .as_mut()
            .and_then(|child| child.try_wait().ok())
            .flatten()
    }

    /// Explicit graceful reap on the launch error path, consuming the guard (its Drop belt is disarmed).
    async fn reap_graceful(mut self, grace: Duration) {
        let Some(mut child) = self.spawned_child.take() else {
            return;
        };
        tracing::warn!(
            target: "shared_codex_daemon::stop",
            pgid = self.pgid,
            "spawn failed; reaping child gracefully"
        );
        terminate_group_with_grace(
            SignalScope::Group { pgid: self.pgid },
            ExitWait::Child(&mut child),
            grace,
        )
        .await;
    }
}

impl Drop for SpawnedChildGuard {
    /// SIGTERM-only belt: fires only on a panic inside, or a runtime-teardown abort of, the
    /// detached transition task. No SIGKILL here: an instant SIGKILL arms codex's backfill lease.
    /// It does NOT terminalize the supervisor; the durable row still names the child.
    fn drop(&mut self) {
        if self.spawned_child.is_none() {
            return;
        }
        tracing::warn!(
            target: "shared_codex_daemon::stop",
            pgid = self.pgid,
            "spawn transition dropped mid-flight (panic/teardown); belt SIGTERM only"
        );
        signal_process_group(self.pgid, libc::SIGTERM);
    }
}

// `impl Drop for SharedCodexAppServer` is DELETED deliberately: calm-server shutdown NEVER
// signals the shared codex daemon; it is left running for the next boot's takeover.

#[async_trait::async_trait]
impl calm_provider::provider::CodexDaemonProbe for SharedCodexAppServer {
    fn is_running(&self) -> bool {
        SharedCodexAppServer::is_running(self)
    }

    fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<String> {
        SharedCodexAppServer::active_turn_id_for_thread(self, thread_id)
            .map(|turn_id| turn_id.to_string())
    }

    fn remote_uri(&self) -> String {
        SharedCodexAppServer::remote_uri(self)
    }

    fn daemon_connected_at_ms(&self) -> calm_types::runtime::TimestampMs {
        SharedCodexAppServer::daemon_connected_at_ms(self)
    }

    /// Pull the liveness facts via `thread/read(include_turns)` (+ `thread/loaded/list`); `None`
    /// on ANY RPC error, which the arbiter treats as `Unknown`.
    async fn read_liveness_facts(
        &self,
        thread_id: &str,
    ) -> Option<calm_provider::provider::CodexLivenessFacts> {
        let client = self.connected_client().await.ok()?;
        let read = client.thread_read(thread_id, true).await.ok()?;
        // Secondary `loaded` signal; a failed list shouldn't sink the pull.
        let loaded = client
            .thread_loaded_list()
            .await
            .ok()
            .map(|ids| ids.iter().any(|id| id == thread_id))
            .unwrap_or(false);
        Some(liveness_facts_from_read(read, loaded))
    }
}

/// Map the wire `thread/read` response (+ `loaded` flag) into [`CodexLivenessFacts`]; the
/// "last turn" is the MOST RECENT element of `turns`.
fn liveness_facts_from_read(
    read: crate::codex_appserver::ThreadReadResponse,
    loaded: bool,
) -> calm_provider::provider::CodexLivenessFacts {
    use crate::codex_appserver::{ThreadActiveFlag, ThreadStatus, TurnStatus};
    use calm_provider::provider::{
        CodexLivenessFacts, LastTurnFacts, ThreadStatusLite, TurnStatusLite,
    };

    let status = match read.thread.status {
        ThreadStatus::NotLoaded => ThreadStatusLite::NotLoaded,
        ThreadStatus::Idle => ThreadStatusLite::Idle,
        ThreadStatus::SystemError => ThreadStatusLite::SystemError,
        ThreadStatus::Active { active_flags } => ThreadStatusLite::Active {
            waiting_on_user_input: active_flags.contains(&ThreadActiveFlag::WaitingOnUserInput),
            waiting_on_approval: active_flags.contains(&ThreadActiveFlag::WaitingOnApproval),
        },
    };
    // `last_turn`: None = no turns present (None or empty list).
    let last_turn = read
        .thread
        .turns
        .as_deref()
        .and_then(|turns| turns.last())
        .map(|turn| LastTurnFacts {
            completed_at: turn.completed_at,
            status: match turn.status {
                TurnStatus::Completed => TurnStatusLite::Completed,
                TurnStatus::Interrupted => TurnStatusLite::Interrupted,
                TurnStatus::Failed => TurnStatusLite::Failed,
                TurnStatus::InProgress => TurnStatusLite::InProgress,
                TurnStatus::Unknown => TurnStatusLite::Unknown,
            },
        });
    CodexLivenessFacts {
        loaded,
        status,
        last_turn,
    }
}

async fn connect_initialized(
    sock: &Path,
) -> Result<(CodexAppServer, crate::codex_appserver::NotificationStream)> {
    let (client, notifications) = CodexAppServer::connect(sock).await?;
    let client = client.with_request_timeout(Duration::from_secs(10));
    client
        .initialize(ClientInfo {
            name: "neige-calm-shared-supervisor".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })
        .await?;
    Ok((client, notifications))
}

#[cfg(any(test, feature = "fixtures"))]
pub fn drop_spawned_child_guard_for_test(child: Child, pgid: i32) {
    let _guard = SpawnedChildGuard::new(child, pgid);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// #1813: the liveness facts carry the last turn's own status next to the thread's, so a
    /// failed turn on a reloaded (`idle`) thread still reads back as failed. Every wire value.
    #[test]
    fn liveness_facts_carry_the_last_turns_own_status() {
        use calm_provider::provider::{LastTurnFacts, ThreadStatusLite, TurnStatusLite};
        for (wire, lite) in [
            ("completed", TurnStatusLite::Completed),
            ("interrupted", TurnStatusLite::Interrupted),
            ("failed", TurnStatusLite::Failed),
            ("inProgress", TurnStatusLite::InProgress),
            ("queued", TurnStatusLite::Unknown),
        ] {
            let read: crate::codex_appserver::ThreadReadResponse = serde_json::from_value(json!({
                "thread": {
                    "status": { "type": "idle" },
                    "turns": [
                        { "completedAt": 1700, "status": "completed" },
                        { "completedAt": 1800, "status": wire }
                    ]
                }
            }))
            .unwrap();
            let facts = liveness_facts_from_read(read, true);
            assert_eq!(facts.status, ThreadStatusLite::Idle);
            assert_eq!(
                facts.last_turn,
                Some(LastTurnFacts {
                    completed_at: Some(1800),
                    status: lite,
                }),
                "{wire}"
            );
        }
    }

    /// Entries leave the tombstone set by FIFO eviction at the cap and by nothing else; a
    /// re-remembered id does not consume a second slot.
    #[test]
    fn forgotten_thread_tombstones_evict_oldest_first_at_the_cap() {
        let mut forgotten = ForgottenThreads::default();
        for n in 0..FORGOTTEN_THREAD_TOMBSTONE_CAP {
            forgotten.remember(&format!("T-{n}"));
        }
        assert!(forgotten.contains("T-0"));
        assert_eq!(forgotten.set.len(), FORGOTTEN_THREAD_TOMBSTONE_CAP);

        // A duplicate is not a new slot: nothing is evicted.
        forgotten.remember("T-0");
        assert_eq!(forgotten.set.len(), FORGOTTEN_THREAD_TOMBSTONE_CAP);
        assert!(forgotten.contains("T-0"));

        // One over the cap evicts exactly the oldest, and only the oldest.
        forgotten.remember("T-overflow");
        assert_eq!(forgotten.set.len(), FORGOTTEN_THREAD_TOMBSTONE_CAP);
        assert!(!forgotten.contains("T-0"), "the oldest must be evicted");
        assert!(forgotten.contains("T-1"));
        assert!(forgotten.contains("T-overflow"));
    }

    /// The dedup cache is updated ONLY after a successful DB write.
    #[test]
    fn failed_persist_dedup_updates_only_after_successful_write() {
        let dedup = FailedPersistDedup::default();
        let identity = FailedIdentity::proven_absent(Path::new("/tmp/s"), Path::new("/tmp/h"));
        assert!(!dedup.should_skip("boom", FailureClass::Persistent, &identity));
        // Simulated DB-write failure: `note_written` is NOT called.
        assert!(
            !dedup.should_skip("boom", FailureClass::Persistent, &identity),
            "a failed write must not populate the dedup cache"
        );
        dedup.note_written("boom".into(), FailureClass::Persistent, identity.clone());
        assert!(
            dedup.should_skip("boom", FailureClass::Persistent, &identity),
            "identical consecutive tuples dedup after a successful write"
        );
        // Any component change re-enables the write.
        assert!(!dedup.should_skip("other", FailureClass::Persistent, &identity));
        assert!(!dedup.should_skip("boom", FailureClass::Transient, &identity));
        let retained = FailedIdentity {
            pid: Some(42),
            ..identity.clone()
        };
        assert!(!dedup.should_skip("boom", FailureClass::Persistent, &retained));
        // A successful non-failed write clears the cache.
        dedup.clear();
        assert!(!dedup.should_skip("boom", FailureClass::Persistent, &identity));
    }

    /// Lane classification: cold-start poll failures and child-exit shapes are Transient;
    /// exec/config/guard failures are Persistent.
    #[test]
    fn classify_spawn_failure_assigns_lanes() {
        let transient = [
            "shared codex app-server exited before initialize (exit status: 7) after 0.1s: x",
            "shared codex app-server not initialized after 2.0s (deadline 2s, child still alive): x",
            "shared codex app-server exited: exit status: 1",
        ];
        for msg in transient {
            assert_eq!(
                classify_spawn_failure(&CalmError::CodexAppServer(msg.into())),
                FailureClass::Transient,
                "{msg}"
            );
        }
        let persistent = [
            "spawn shared codex app-server: No such file or directory (os error 2)",
            "refusing to launch shared codex app-server: unexpected mcp server evil",
            "db error: settings read failed",
        ];
        for msg in persistent {
            assert_eq!(
                classify_spawn_failure(&CalmError::CodexAppServer(msg.into())),
                FailureClass::Persistent,
                "{msg}"
            );
        }
    }

    /// Jitter stays within ±20%.
    #[test]
    fn heal_jitter_stays_within_twenty_percent() {
        for _ in 0..64 {
            let jittered = heal_jitter(Duration::from_secs(100));
            assert!(
                jittered >= Duration::from_secs(80) && jittered <= Duration::from_secs(120),
                "jittered delay {jittered:?} outside ±20% of 100s"
            );
        }
    }

    /// Persistent hits the slow ceiling with a floor at `restart_max_delay`; Transient stays
    /// on the fast lane cap.
    #[test]
    fn slow_lane_floors_at_restart_max_and_caps_at_const_ceiling() {
        let state = BackoffState::new(Duration::from_millis(250), Duration::from_secs(10));
        assert_eq!(
            state.next_slow_delay(HEAL_SLOW_RETRY_CEILING),
            Duration::from_secs(10),
            "slow-lane floor must be restart_max_delay"
        );
        let mut last = Duration::ZERO;
        for _ in 0..20 {
            last = state.next_slow_delay(HEAL_SLOW_RETRY_CEILING);
        }
        assert_eq!(
            last, HEAL_SLOW_RETRY_CEILING,
            "slow-lane ceiling must be the 300s const"
        );

        let fast = BackoffState::new(Duration::from_millis(250), Duration::from_secs(10));
        let mut last_fast = Duration::ZERO;
        for _ in 0..20 {
            last_fast = fast.next_delay();
        }
        assert_eq!(
            last_fast,
            Duration::from_secs(10),
            "the Transient lane keeps the configured fast cap"
        );
    }

    /// Read-side classification rule over row shapes.
    #[test]
    fn failed_row_identity_presence_classifies_unreconciled() {
        let mut record = crate::db::SharedCodexDaemonRecord {
            state: "failed".into(),
            pid: None,
            pgid: None,
            sock_path: None,
            codex_home_path: None,
            process_start_time: None,
            boot_id: None,
            started_at: None,
            updated_at: 0,
            restart_count: 0,
            last_error: None,
            daemon_env_signature: None,
        };
        assert!(
            !failed_row_identity_present(&record),
            "failed + all-NULL identity is SafeToRetry (legacy rows included)"
        );
        record.boot_id = Some("b".into());
        assert!(
            failed_row_identity_present(&record),
            "ANY identity column present marks the row unreconciled"
        );
        record.state = "running".into();
        assert!(
            !failed_row_identity_present(&record),
            "the rule applies to failed rows only"
        );
    }

    /// A settings-read failure inside `try_takeover_live` must not escape with the in-memory
    /// state stranded `Restarting`; the DB row is left untouched. Injection: drop ONLY the
    /// `settings` table; the "live verified daemon" is this test process itself.
    #[tokio::test]
    async fn takeover_settings_read_failure_terminalizes_failed_and_arms_heal() {
        use calm_truth::db::{RepoOutOfDomain as _, RepoRead as _};
        let repo = Arc::new(
            crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
                .await
                .unwrap(),
        );
        sqlx::query("DROP TABLE settings")
            .execute(repo.pool())
            .await
            .unwrap();

        let self_pid = i32::try_from(std::process::id()).unwrap();
        let start_time = read_proc_start_time(self_pid).expect("own start time");
        let boot_id = read_boot_id().unwrap_or_default();
        repo.shared_daemon_runtime_set(SharedCodexDaemonUpdate {
            state: SharedDaemonState::Running.as_db_str().to_string(),
            pid: Some(self_pid),
            pgid: Some(self_pid),
            sock_path: Some("/tmp/none.sock".into()),
            codex_home_path: None,
            process_start_time: Some(start_time),
            boot_id: Some(boot_id),
            started_at: Some(now_ms()),
            last_error: None,
            increment_restart_count: false,
            daemon_env_signature: Some("stale".into()),
        })
        .await
        .unwrap();

        let root = tempfile::tempdir().unwrap();
        let cfg = <crate::config::Config as clap::Parser>::parse_from([
            "calm-server",
            "--data-dir",
            root.path().to_str().unwrap(),
        ]);
        let home = crate::shared_codex_home::SharedCodexHome::new(
            cfg.data_dir_resolved().join("codex-home"),
            cfg.data_dir_resolved().join("codex-homes"),
        );
        home.seed().unwrap();
        let daemon = SharedCodexAppServer::new(&cfg, Arc::new(home), repo.clone());

        let err = daemon
            .ensure_running()
            .await
            .expect_err("the settings-read failure must fail the transition");
        assert!(
            err.to_string().contains("settings"),
            "error must name the settings read; got: {err}"
        );

        let snapshot = daemon.status_snapshot();
        assert_eq!(
            snapshot.state,
            SharedDaemonState::Failed,
            "the serial must release on a TERMINAL in-memory state, never \
             stranded Restarting; got {:?}",
            snapshot.state
        );
        assert!(
            snapshot
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("settings"),
            "the Failed state must carry the settings-read failure"
        );
        assert!(
            daemon.heal_active_for_test(),
            "the escape path must arm the heal loop"
        );

        // Row untouched: it still truthfully names the live verified daemon.
        let record = repo.shared_daemon_runtime_get().await.unwrap();
        assert_eq!(
            SharedDaemonState::from_db_str(&record.state),
            SharedDaemonState::Running,
            "no forged failed/absence write over the live daemon's row"
        );
        assert_eq!(record.pid, Some(self_pid), "identity untouched");

        // The supervisor is not wedged: another round runs to the same
        // terminal state (no stuck serial, no stranded Restarting).
        daemon
            .ensure_running()
            .await
            .expect_err("still failing while the settings table is gone");
        assert_eq!(daemon.status_snapshot().state, SharedDaemonState::Failed);
    }

    #[test]
    fn shared_thread_start_params_debug_scrubs_neige_mcp_token() {
        let params = SharedThreadStartParams {
            cwd: "/workspace".into(),
            approval_policy: "never".into(),
            sandbox_mode: "workspace-write".into(),
            developer_instructions: None,
            config: ThreadConfig::McpShell {
                role: CardRole::Planner,
                socket_path: PathBuf::from("/tmp/x.sock"),
                raw_token: "secret-abcdef".into(),
            },
        };

        let rendered = format!("{params:?}");
        assert!(!rendered.contains("secret-abcdef"));
        assert!(rendered.contains("NEIGE_MCP_TOKEN"));
        assert!(rendered.contains("\"[REDACTED]\""));
    }

    #[test]
    fn thread_id_from_started_accepts_real_codex_object_shape() {
        let params = json!({
            "thread": {"id": "thrd_abc"},
            "turn_id": "turn_1",
        });
        assert_eq!(thread_id_from_started(&params), Some("thrd_abc"));
    }

    #[test]
    fn thread_id_from_started_accepts_flat_shape_for_compat() {
        let params = json!({"threadId": "thrd_xyz"});
        assert_eq!(thread_id_from_started(&params), Some("thrd_xyz"));
    }

    /// The schema salt must change the signature for identical inputs.
    #[test]
    fn env_signature_salt_differs_from_pre_salt_signature() {
        let ingest = "http://127.0.0.1:8765";
        let mut h = Sha256::new();
        h.update(ingest.as_bytes());
        h.update(b"|");
        h.update(b"|");
        let pre_salt = hex::encode(h.finalize())[..16].to_string();

        let salted =
            SharedCodexAppServer::compute_env_signature(ingest, None, None, Path::new("/k/bin"));
        assert_ne!(
            salted, pre_salt,
            "compute_env_signature must be salted (env-schema-v3:1784)"
        );
    }

    /// With `env_clear()`, a parent-env proxy must be RESOLVED and set explicitly when settings
    /// are absent; settings still win over the parent env.
    #[test]
    fn resolved_proxy_pairs_settings_first_then_explicit_parent_env_fallback() {
        let pairs = SharedCodexAppServer::resolved_proxy_env_pairs(
            Some("http://settings-proxy:3128"),
            None,
            |key| (key == "HTTP_PROXY").then(|| "http://env-proxy:8080".to_string()),
        );
        assert_eq!(
            pairs,
            vec![(
                "HTTP_PROXY",
                "http_proxy",
                "http://settings-proxy:3128".to_string()
            )]
        );

        let pairs = SharedCodexAppServer::resolved_proxy_env_pairs(None, None, |key| {
            (key == "HTTPS_PROXY").then(|| "http://env-secure:3129".to_string())
        });
        assert_eq!(
            pairs,
            vec![(
                "HTTPS_PROXY",
                "https_proxy",
                "http://env-secure:3129".to_string()
            )]
        );

        assert!(
            SharedCodexAppServer::resolved_proxy_env_pairs(None, None, |_| None).is_empty(),
            "no settings + no parent env => no proxy keys in the child env"
        );
    }
}
