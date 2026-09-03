//! Shared `codex app-server` supervisor (#410 PR4).
//!
//! PR4 only starts, supervises, and takes over a single daemon for the whole
//! server. It deliberately does not route any card traffic to this daemon yet;
//! later PRs switch callers over through the public methods here.

use std::collections::HashSet;
use std::os::unix::io::AsRawFd;
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
    ClientInfo, CodexAppServer, InputItem, Notification, ThreadStartParams,
    redact_thread_start_config,
};
use crate::config::Config;
use crate::db::sqlite::session_projection_active_for_card_tx;
use crate::db::{Repo, SharedCodexDaemonUpdate, write_in_tx_typed};
use crate::error::{CalmError, Result};
use crate::mcp_server::transport;
use crate::mcp_server::wiring::{card_mcp_thread_start_config, mint_and_persist_card_token};
use crate::model::{CardRole, now_ms};
use crate::pending_codex_threads::PendingThreadStartRegistry;
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

/// #863 — ambient env keys forwarded verbatim into the spawned shared codex
/// app-server. A const, not config: it is an implementation invariant of the
/// spawn seam, not an operator choice; changing it is a code change with
/// review + tests. Everything else in the parent env is dropped by
/// `env_clear()`; computed keys (CODEX_HOME, NEIGE_CALM_BASE_URL, HTTP(S)
/// proxies) are set explicitly in `apply_spawn_env`. Entry rationale cites
/// the vendored codex source (`external/codex/codex-rs`).
pub const SPAWN_ENV_PASSTHROUGH: &[&str] = &[
    // binary/tool resolution + codex arg0 PATH rewrite (codex-rs/arg0/src/lib.rs:146-153);
    // MCP child commands are NOT which-resolved on unix — child PATH is used
    // (codex-rs/rmcp-client/src/program_resolver.rs:22-29, utils.rs:130-142)
    "PATH",
    // default-home fallback + `~` expansion in config paths (codex-rs/utils/home-dir/src/lib.rs:52-61,
    // codex-rs/utils/absolute-path/src/lib.rs:29); forwarded to MCP children (utils.rs:130-142)
    "HOME",
    // codex's own child allow-lists forward these (rmcp-client/src/utils.rs:130-142 for MCP;
    // UNIX_CORE_ENV_VARS protocol/src/shell_environment.rs:113-116 for inherit=Core shells)
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
    // (codex-rs/login/src/auth/default_client.rs:222-229; not sandboxed → proxies active :250-251)
    "NO_PROXY",
    "no_proxy",
    "ALL_PROXY",
    "all_proxy",
    // TLS custom CA (codex-rs/codex-client/src/custom_ca.rs:61-62, 373-386; SSL_CERT_DIR unused)
    "CODEX_CA_CERTIFICATE",
    "SSL_CERT_FILE",
    // diagnostics (codex-rs/app-server/src/lib.rs:627,632 RUST_LOG; :112 LOG_FORMAT)
    "RUST_LOG",
    "LOG_FORMAT",
    "RUST_BACKTRACE",
    // API-key-mode auth fallbacks (codex-rs/login/src/auth/manager.rs:516-532;
    // model-provider-info/src/lib.rs:340-347). Prod uses auth.json; kept so an
    // API-key deployment doesn't silently break. Explicit pass-through, still an allow-list.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeMode {
    ColdRespawn,
    HotTakeover,
}

/// #953 §2 — slow-lane retry ceiling for the self-heal loop. A code
/// invariant, not a knob (Explicit-Config rule: no new config knobs).
pub const HEAL_SLOW_RETRY_CEILING: Duration = Duration::from_secs(300);

/// #953 §2 — ±20% uniform jitter applied to every heal delay.
const HEAL_JITTER_FRACTION: f64 = 0.2;

/// #953 §2 — classified failure lanes for the self-heal loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Fast lane — the existing `BackoffState` knobs. Child exited,
    /// handshake/cold-start deadline, socket errors.
    Transient,
    /// Slow lane — spawn exec error, settings/config read error, guard
    /// refusal that is safe to retry (floor `restart_max_delay_ms`, ceiling
    /// [`HEAL_SLOW_RETRY_CEILING`]).
    Persistent,
    /// Slow lane, reconciliation-only rounds (#953 §3): a possible surviving
    /// daemon we could not prove gone. The spawn path is unreachable until a
    /// reconciliation round proves absence (or, for the pid-NULL operator
    /// shape, until the operator clears the identity columns).
    Unreconciled,
}

/// #953 §1 — precondition validated (before any destructive work) by
/// [`SharedCodexAppServer::transition_replace`] under the same core lock
/// that captures the running parts. The transition serial is never released
/// between validation and the terminal Running/Failed write, so no second
/// pre-spawn check is needed.
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

/// #953 §5 — readiness snapshot published on every installed Running /
/// terminal Failed, and on every transition ENTRY (`running: false` with
/// the outgoing generation — PR2 review D1(b)), so `running: true` holds
/// only while a Running incarnation is actually installed. `generation`
/// identifies the Running incarnation (equality comparisons only;
/// wrap/restart-reset harmless for the process-local consumer). Sole
/// consumer in this slice: deferred harness recovery; future consumers
/// (status/health, admin wait) can watch the same channel.
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

/// #953 §3 — result of the exhaustive per-shape resolution over a persisted
/// daemon record.
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

/// #953 §2 — the identity/context columns a Failed persist writes. NULL-only
/// when absence is proven; retained exactly as read while unreconciled.
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

    /// Unreconciled: retain the tuple exactly as read (full or partial) —
    /// identity presence IS the durable unreconciled marker (#953 §3).
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

/// #953 §2 — last successfully-persisted Failed tuple. Identical consecutive
/// Failed writes are skipped; the cache is updated ONLY after the DB write
/// returns Ok (`note_written`), so a failed write is retried next round,
/// never masked. Any successful non-failed write clears it.
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

/// #953 §2 — RAII claim for the singleton heal task: `Drop` clears the
/// `heal_active` flag, so panic, cancellation, abort, and normal exit all
/// release the claim.
struct HealActiveGuard {
    flag: Arc<AtomicBool>,
}

impl Drop for HealActiveGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

/// #953 §2 — failure classification for the heal lanes. The Transient
/// messages are exactly the cold-start poll failures produced in this file
/// (`poll_connect_initialized`) plus the crash-watcher exit shape; everything
/// else (exec/spawn errors, settings/config read errors, guard refusals)
/// retries on the slow lane.
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

/// #954 review D2 — marker for the ONE error whose transition outcome was
/// never observed (the detached spawn task died without publishing its
/// result). Callers must not claim "terminalized + heal armed" for it: the
/// in-memory state may still be `Starting` with heal unarmed.
const DETACHED_SPAWN_RESULT_LOST: &str =
    "detached spawn transition task ended without publishing a result";

fn detached_spawn_result_lost(err: &CalmError) -> bool {
    err.to_string().contains(DETACHED_SPAWN_RESULT_LOST)
}

/// #953 §2 — ±20% uniform jitter. Cheap clock-derived entropy: the heal loop
/// only needs desynchronization, not statistical quality (no rand dep).
fn heal_jitter(delay: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    let unit = (nanos % 1_000_001) as f64 / 1_000_000.0;
    delay.mul_f64(1.0 - HEAL_JITTER_FRACTION + 2.0 * HEAL_JITTER_FRACTION * unit)
}

/// `/proc/<pid>/stat` state == `Z`. #953 review D4 — a zombie's meaning
/// depends on WHICH question is being asked: for post-reap verification of
/// a group WE just signaled it means "dead" (the leader cannot serve and
/// merely awaits its parent's `wait()`, which may not be us — e.g. a
/// test-spawned survivor); for a reconciliation probe BEFORE any signal it
/// proves nothing about the process GROUP — live descendants may remain in
/// the zombie leader's group, so absence must never be claimed from it.
/// See [`survivor_alive_after_group_reap`] (post-signal, zombie = dead) vs
/// [`proc_pid_present`] / `verify_owned_pid` (pre-signal, zombie = present).
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

/// `/proc/<pid>` presence — the ONLY probe allowed for the pid-partial
/// shape (#953 §3 shape (a)): ownership is unprovable (the pid may be
/// recycled), so the record must never be reaped or signaled. #953 review
/// D4(b) — a ZOMBIE counts as present: this probe runs before (and without)
/// any signal, and a zombie entry does not prove the group named by the
/// unverifiable record is gone. The shape self-resolves when the zombie's
/// parent reaps it (`/proc/<pid>` disappears).
fn proc_pid_present(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    Path::new(&format!("/proc/{pid}")).exists()
}

/// #953 §3 — POST-group-reap "survivor still alive" criterion: the verified
/// identity matches a live, non-zombie process. Zombie-as-dead is valid
/// here ONLY because every caller has just signaled the verified group
/// (SIGTERM + SIGKILL): a leader left zombie by our own SIGKILL is dead for
/// supervision purposes and merely awaits its parent's `wait()` (which may
/// not be us, e.g. a test-spawned survivor). Absence proofs BEFORE any
/// signal must NOT use this predicate (#953 review D4): pre-signal probes
/// are `verify_owned_pid` (full tuple) or [`proc_pid_present`]
/// (pid-partial shape), both of which treat a zombie as still-present.
fn survivor_alive_after_group_reap(pid: i32, start_time: u64, boot_id: &str) -> bool {
    verify_owned_pid(pid, start_time, boot_id) && !proc_pid_is_zombie(pid)
}

/// #953 §3 durable marker prefixes — human-readable belt-and-braces; the
/// machine rule is identity presence (`failed_row_identity_present`).
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

/// #953 §3 read-side classification rule (explicit; survives calm-server
/// restarts): `state='failed'` ∧ all identity columns NULL ⇒ SafeToRetry;
/// `state='failed'` ∧ any identity column present ⇒ Unreconciled — run the
/// per-shape resolution algorithm before any spawn. Unambiguous vs legacy
/// rows: every pre-#953 Failed writer NULLed identity.
fn failed_row_identity_present(record: &crate::db::SharedCodexDaemonRecord) -> bool {
    SharedDaemonState::from_db_str(&record.state) == SharedDaemonState::Failed
        && (record.pid.is_some()
            || record.pgid.is_some()
            || record.process_start_time.is_some()
            || record.boot_id.is_some())
}

/// The Failed message written while a row stays unreconciled. A pure
/// function of the record's identity shape — NOT of its previous
/// `last_error` — so identical consecutive rounds produce an identical
/// tuple and dedup to a single DB write.
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
        socket_path: PathBuf,
        raw_token: String,
    },
}

impl ThreadConfig {
    fn to_wire_config(&self) -> Option<serde_json::Value> {
        match self {
            Self::NoMcp => None,
            Self::McpShell {
                socket_path,
                raw_token,
            } => Some(card_mcp_thread_start_config(socket_path, raw_token)),
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
        let redacted_config = self.config.to_wire_config();
        f.debug_struct("SharedThreadStartParams")
            .field("cwd", &self.cwd)
            .field("approval_policy", &self.approval_policy)
            .field("sandbox_mode", &self.sandbox_mode)
            .field("developer_instructions", &self.developer_instructions)
            .field("config", &redact_thread_start_config(&redacted_config))
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

    /// #953 §2 — slow heal lane: same attempts counter and stable-window
    /// semantics as [`Self::next_delay`], but with floor = this state's max
    /// (`restart_max_delay_ms`) and a caller-supplied ceiling.
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

pub type NotificationFanout = broadcast::Sender<Notification>;

pub struct SharedCodexAppServer {
    sock: PathBuf,
    kernel_mcp_socket_path: PathBuf,
    home: Arc<SharedCodexHome>,
    repo: Arc<dyn Repo>,
    thread_cache: Arc<DashMap<String, String>>,
    active_turns: Arc<DashMap<String, String>>,
    restart_backoff: BackoffState,
    /// #949 — cold-start deadline for a freshly spawned child to bind its
    /// socket and answer `initialize`. Codex may spend minutes backfilling
    /// its state db before the socket exists; only this deadline (or a dead
    /// child) may fail the spawn.
    start_timeout: Duration,
    /// #954 — stop-grace ceiling for every daemon termination: SIGTERM →
    /// exit-driven wait up to this ceiling → straggler cleanup (group
    /// SIGKILL where the group is provably pinned, else the per-member
    /// identity sweep — see `terminate_group_with_grace`). A cooperative
    /// daemon pays its actual exit time, never the full grace.
    stop_grace: Duration,
    notifications: NotificationFanout,
    pending_codex_threads_handle: Option<Arc<PendingThreadStartRegistry>>,
    kernel_initiated_threads: Arc<Mutex<HashSet<String>>>,
    kernel_thread_start_serial: Arc<Mutex<()>>,
    codex_bin: String,
    log_dir: PathBuf,
    restart_count: std::sync::atomic::AtomicU64,
    /// Wall-clock ms of the most recent successful daemon (re)connect (#741
    /// §1.3). Stamped in `install_client` (the common connect/respawn path).
    /// Feeds the 741-3 reaper's `REBUILD_GRACE`; nothing consumes it yet. `0`
    /// until the first connect.
    daemon_connected_at_ms: AtomicI64,
    needs_respawn_on_next_thread_start: Arc<AtomicBool>,
    /// #480 §C — typestate-companion state machine. PR5b migrates readers.
    core: Arc<tokio::sync::Mutex<SupervisorCore>>,
    /// #480 §C — serializes process transitions (replaces `restart_lock` in PR5b).
    transition_serial: Arc<tokio::sync::Mutex<()>>,
    /// #953 §2 — singleton claim for the background heal task; cleared by
    /// the RAII [`HealActiveGuard`] on any task exit (panic/abort included).
    heal_active: Arc<AtomicBool>,
    /// #953 §2 — slow-lane immediate wake on external change (the existing
    /// settings-change path); the heal loop sleeps in select(sleep, nudge).
    heal_nudge: Arc<tokio::sync::Notify>,
    /// #953 §2 — post-Ok dedup of Failed DB writes.
    failed_persist_dedup: FailedPersistDedup,
    /// #953 §5 — readiness channel: stamped on every installed Running
    /// (typestate success arm) and every terminal Failed (typestate error
    /// arm + the in-memory terminalization paths), and INVALIDATED
    /// (`running: false`, outgoing generation) on transition ENTRY
    /// (`transition_replace` leaving Running — PR2 review D1(b)), so a
    /// transitional Restarting/Starting daemon is never mistaken for the
    /// Running incarnation it replaced. Receivers via
    /// [`Self::readiness_receiver`].
    readiness: tokio::sync::watch::Sender<DaemonReadiness>,
    /// #953 review D1 — fixtures-only interleaving gate: the heal loop's Ok
    /// path awaits this mutex AFTER `ensure_running` succeeded but BEFORE it
    /// releases the singleton claim. A test holding the lock parks the heal
    /// task inside that window (modelling the post-serial
    /// `resume_cached_threads` seconds) so a concurrent crash + failed
    /// restart can deterministically lose its `schedule_heal` CAS against
    /// the held claim. Uncontended (a lock/unlock pair) when no test holds
    /// it; absent from non-fixtures builds.
    #[cfg(feature = "fixtures")]
    heal_post_ok_gate: Arc<tokio::sync::Mutex<()>>,
    /// #953 PR2 review D1 — fixtures-only interleaving gate:
    /// `transition_replace` awaits this mutex right after the transition
    /// ENTRY (state captured/replaced + readiness invalidated) and before
    /// the reap/start body. A test holding the lock parks the transition
    /// inside that window so the entry-time readiness invalidation is
    /// deterministically observable. Uncontended (a lock/unlock pair) when
    /// no test holds it; absent from non-fixtures builds.
    #[cfg(feature = "fixtures")]
    transition_entry_gate: Arc<tokio::sync::Mutex<()>>,
    /// #954 — test seam: abort handle of the most recent detached spawn
    /// transition task, so tests can model a runtime-teardown/panic abort
    /// of the transition itself (caller cancellation can no longer reach
    /// it). Fixtures builds only.
    #[cfg(feature = "fixtures")]
    detached_spawn_task: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
    ingest_url: String,
    #[cfg(feature = "fixtures")]
    fake: Option<Arc<FakeSharedCodexAppServer>>,
}

#[cfg(feature = "fixtures")]
pub type StartedThreadParam = (Option<String>, bool, Option<CardRole>);

#[cfg(feature = "fixtures")]
pub struct FakeSharedCodexAppServer {
    next_thread: AtomicU64,
    next_turn: AtomicU64,
    fail_next_thread_start: AtomicBool,
    started_thread_params: std::sync::Mutex<Vec<StartedThreadParam>>,
    started_turns: std::sync::Mutex<Vec<(String, Vec<InputItem>)>>,
    interrupted_turns: std::sync::Mutex<Vec<(String, String)>>,
}

#[cfg(feature = "fixtures")]
impl FakeSharedCodexAppServer {
    fn new() -> Self {
        Self {
            next_thread: AtomicU64::new(1),
            next_turn: AtomicU64::new(1),
            fail_next_thread_start: AtomicBool::new(false),
            started_thread_params: std::sync::Mutex::new(Vec::new()),
            started_turns: std::sync::Mutex::new(Vec::new()),
            interrupted_turns: std::sync::Mutex::new(Vec::new()),
        }
    }
}

/// #480 PR5a — typestate companion to the existing `SharedDaemonState`.
/// Carries process-ownership data per variant; PR5b will migrate readers
/// and remove the old scattered fields.
///
/// Hard boundaries (§F):
/// - `Child` MUST stay private; only transition APIs may kill/replace.
/// - Sibling attribution (thread_cache, active_turns, pending) is NOT
///   part of typestate — those survive process restarts.
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
        /// #953 §2 — heal-lane classification for this failure.
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
    /// #953 §1 — bumped (`wrapping_add`, defined wrap, no debug-overflow
    /// panic) each time a Running state is installed. Consumers compare by
    /// equality only, never ordering: the crash watcher records the
    /// generation it observed dying and passes
    /// [`ReplacePrecondition::GenerationIs`]; a mismatch means another
    /// transition already replaced that process.
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

// ===================== Construction & public thread/turn API =====================
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
            sock: root.join("run/codex-appserver.sock"),
            kernel_mcp_socket_path: transport::default_socket_path(&root),
            home,
            repo,
            thread_cache: Arc::new(DashMap::new()),
            active_turns: Arc::new(DashMap::new()),
            restart_backoff: BackoffState::new(Duration::from_millis(250), Duration::from_secs(10)),
            start_timeout: Duration::from_secs(120),
            stop_grace: Duration::from_secs(60),
            notifications: tx,
            pending_codex_threads_handle,
            kernel_initiated_threads: Arc::new(Mutex::new(HashSet::new())),
            kernel_thread_start_serial: Arc::new(Mutex::new(())),
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
        let data_dir = cfg.data_dir_resolved();
        let (tx, _) = broadcast::channel(1024);
        Arc::new(Self {
            sock: data_dir.join("run/codex-appserver.sock"),
            kernel_mcp_socket_path: transport::default_socket_path(&data_dir),
            home,
            repo,
            thread_cache: Arc::new(DashMap::new()),
            active_turns: Arc::new(DashMap::new()),
            restart_backoff: BackoffState::new(
                Duration::from_millis(cfg.shared_codex_appserver_restart_initial_delay_ms),
                Duration::from_millis(cfg.shared_codex_appserver_restart_max_delay_ms),
            ),
            start_timeout: Duration::from_secs(cfg.shared_codex_appserver_start_timeout_secs),
            stop_grace: Duration::from_secs(cfg.shared_codex_appserver_stop_grace_secs),
            notifications: tx,
            pending_codex_threads_handle,
            kernel_initiated_threads: Arc::new(Mutex::new(HashSet::new())),
            kernel_thread_start_serial: Arc::new(Mutex::new(())),
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

    pub fn codex_home_path(&self) -> &std::path::Path {
        self.home.path()
    }

    pub async fn start_or_takeover(self: &Arc<Self>) -> Result<()> {
        // #863 review R2-1 / #953 §1 — the whole boot sequence is ONE
        // serialized lifecycle transition: `transition_serial` is held from
        // BEFORE the boot guard through the guard-refusal reconciliation +
        // Failed write AND through the takeover/spawn (#480 §C invariant).
        // The downstream callees (`start_body_locked` → `try_takeover_live`
        // → `start_new_process_typestate`) are `_locked`-style callees that
        // assume this guard instead of re-acquiring it — no double-lock.
        // Thread resume runs after the serial is released (#953 §1: not a
        // transition).
        //
        // #954 — the serial is an OWNED guard threaded down the `_locked`
        // chain as a by-value token (type-enforcing the held-serial
        // invariant); the spawn path moves it into the detached transition
        // task, which releases it only after the terminal Running/Failed
        // write.
        let serial = Arc::clone(&self.transition_serial).lock_owned().await;
        self.rebuild_thread_cache_from_db().await?;
        let via = self.start_body_locked(serial, false, None).await?;
        self.resume_cached_threads(via.resume_mode()).await;
        Ok(())
    }

    /// #953 §3 — supervisor entry to (re)establish a running daemon from any
    /// non-Running state: `start_or_takeover` with a Running fast-path,
    /// sharing the same `_locked` body (guard verify → persisted-record
    /// reconciliation → takeover probe → spawn). The `NotRunning`
    /// precondition is re-validated under the transition serial, so the heal
    /// loop can never stomp a Running daemon a concurrent transition won.
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

    /// #953 §5 — subscribe to the readiness channel (stamped on every
    /// installed Running / terminal Failed, and INVALIDATED — `running:
    /// false`, outgoing generation — on transition ENTRY in
    /// `transition_replace`). Sole consumer in this slice: deferred harness
    /// recovery.
    pub fn readiness_receiver(&self) -> tokio::sync::watch::Receiver<DaemonReadiness> {
        self.readiness.subscribe()
    }

    /// Fixtures-only: publish a readiness value directly, so stub-daemon
    /// tests can drive the deferred-recovery consumer deterministically
    /// without a real spawn/heal cycle.
    #[cfg(feature = "fixtures")]
    pub fn publish_readiness_for_test(&self, generation: u64, running: bool) {
        self.readiness.send_replace(DaemonReadiness {
            generation,
            running,
        });
    }

    /// #953 — preflight enrichment: same error variants/status as before,
    /// message now carries the live failure and the fact that recovery runs
    /// in the background. Preflights stay non-blocking.
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

    /// Kernel-only thread mint. Performs the codex `thread/start` RPC and
    /// populates in-memory caches without touching durable runtime rows;
    /// callers that need a durable card/thread row persist it in their own
    /// transaction boundary.
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

    /// Worker/planner mint that can only inject per-card MCP shell credentials.
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
        let config = params.config.to_wire_config();
        let thread = client
            .thread_start_with_params(ThreadStartParams {
                cwd: params.cwd,
                approval_policy: params.approval_policy,
                sandbox_mode: params.sandbox_mode,
                developer_instructions: params.developer_instructions,
                config,
            })
            .await?;
        let thread_id = thread
            .thread_id()
            .ok_or_else(|| CalmError::CodexAppServer("thread/start returned no thread.id".into()))?
            .to_string();
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

    /// ARCH INVARIANT (#550 F3): planner-harness reconciliation turn issuance
    /// goes through `harness::run_loop::IssueTurnHandle`; direct callers here
    /// are non-harness boot/operation paths or lower-level tests.
    pub async fn turn_start(&self, thread_id: &str, items: Vec<InputItem>) -> Result<TurnId> {
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
            let n = fake.next_turn.fetch_add(1, Ordering::SeqCst);
            let turn_id = format!("fake-turn-{n:04}");
            fake.started_turns
                .lock()
                .expect("fake shared codex started turns mutex poisoned")
                .push((thread_id.to_string(), items.clone()));
            self.active_turns
                .insert(thread_id.to_string(), turn_id.clone());
            let _ = self.notifications.send(Notification::TurnStarted {
                thread_id: thread_id.to_string(),
                turn: serde_json::json!({ "id": turn_id, "input_len": items.len() }),
            });
            return Ok(turn_id);
        }
        let client = self.connected_client().await?;
        let turn = client.turn_start(thread_id, items).await?;
        let turn_id = turn
            .turn_id()
            .map(ToOwned::to_owned)
            .ok_or_else(|| CalmError::CodexAppServer("turn/start returned no turn.id".into()))?;
        self.active_turns
            .insert(thread_id.to_string(), turn_id.clone());
        Ok(turn_id)
    }

    pub async fn turn_interrupt(&self, thread_id: &str, turn_id: &str) -> Result<()> {
        #[cfg(feature = "fixtures")]
        if let Some(fake) = self.fake.as_ref() {
            fake.interrupted_turns
                .lock()
                .expect("fake shared codex interrupted turns mutex poisoned")
                .push((thread_id.to_string(), turn_id.to_string()));
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

    /// Wall-clock ms of the most recent successful daemon (re)connect (#741
    /// §1.3). `0` before the first connect. Stamped in `install_client`.
    pub fn daemon_connected_at_ms(&self) -> calm_types::runtime::TimestampMs {
        self.daemon_connected_at_ms.load(Ordering::SeqCst)
    }

    pub fn mark_needs_respawn(&self) {
        self.needs_respawn_on_next_thread_start
            .store(true, Ordering::SeqCst);
        // #953 §2 — the existing settings-change path doubles as the heal
        // loop's slow-lane immediate wake. `notify_one` stores a permit, so
        // a nudge fired while the loop is mid-round is not lost.
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

// ===================== Env / config derivation =====================
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
    ) -> String {
        let mut h = Sha256::new();
        // #863 — schema-version salt. The first boot of an upgraded binary
        // mismatches every pre-upgrade persisted signature, so the existing
        // reap-for-respawn takeover path (`try_takeover_live`) is guaranteed
        // to replace a daemon spawned with the old (leaky) inherited env.
        h.update(b"env-schema-v2:863|");
        h.update(ingest_url.as_bytes());
        h.update(b"|");
        h.update(http_proxy.unwrap_or_default().as_bytes());
        h.update(b"|");
        h.update(https_proxy.unwrap_or_default().as_bytes());
        let hex = hex::encode(h.finalize());
        hex[..16].to_string()
    }

    /// #863 review F4 — one settings snapshot per spawn: both the child's
    /// proxy env AND the persisted env signature must derive from the SAME
    /// settings read, otherwise a settings change between two loads persists
    /// a signature for an env the child never got.
    async fn load_spawn_env_snapshot(&self) -> Result<SpawnEnvSnapshot> {
        let settings = load_settings(self.repo.as_ref()).await?;
        Ok(SpawnEnvSnapshot {
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
        )
    }

    async fn current_env_signature(&self) -> Result<String> {
        Ok(self.env_signature_for_snapshot(&self.load_spawn_env_snapshot().await?))
    }

    /// Settings-first, parent-env-fallback proxy resolution — the same
    /// resolution `compute_env_signature` hashes — shaped as explicit
    /// (UPPER, lower, value) pairs for the spawn env. #863: with
    /// `env_clear()`, the fallback must be SET explicitly instead of the old
    /// implicit inheritance-when-settings-absent.
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

    /// #863 — the child env is a pure function of typed config: `env_clear()`
    /// plus exactly [`SPAWN_ENV_PASSTHROUGH`], the computed keys, and (in
    /// fixture builds only) the fake-codex fixture channel. The old
    /// `env_remove` of per-card `NEIGE_*` keys is subsumed by `env_clear`.
    /// Proxies come from the caller's pre-resolved [`SpawnEnvSnapshot`] so
    /// the spawn env and the persisted signature share one settings read.
    fn apply_spawn_env(&self, cmd: &mut Command, snapshot: &SpawnEnvSnapshot) {
        cmd.env_clear();
        for key in SPAWN_ENV_PASSTHROUGH {
            // var_os: a non-UTF8 value must pass through, not be silently dropped
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }

        // Fixture channel (test-only passthrough): the fake app-server reads
        // `FAKE_CODEX_*` / `NEIGE_OSC_TRACE_PATH` from its own process env;
        // integration tests set them on the test process and rely on them
        // reaching the child through this real spawn path. Compiled out of
        // production builds — these names must NEVER join the prod
        // `SPAWN_ENV_PASSTHROUGH` const.
        #[cfg(feature = "fixtures")]
        for (key, value) in std::env::vars_os() {
            let fixture_key = key
                .to_str()
                .is_some_and(|k| k.starts_with("FAKE_CODEX_") || k == "NEIGE_OSC_TRACE_PATH");
            if fixture_key {
                cmd.env(&key, value);
            }
        }

        cmd.env("CODEX_HOME", self.home.path())
            .env("NEIGE_CALM_BASE_URL", &self.ingest_url);

        // The snapshot values are already settings-first/parent-env-fallback
        // resolved (`effective_proxy_env` in `load_spawn_env_snapshot`);
        // `resolved_proxy_env_pairs` only applies the (UPPER, lower) pair
        // shaping + empty filter here, so the lookup is inert.
        for (upper, lower, value) in Self::resolved_proxy_env_pairs(
            snapshot.http_proxy.as_deref(),
            snapshot.https_proxy.as_deref(),
            |_| None,
        ) {
            cmd.env(upper, &value).env(lower, value);
        }
    }
}

/// #863 review F4 — a single-settings-read snapshot of the spawn-relevant
/// runtime settings. Both the child's proxy env and the persisted
/// `daemon_env_signature` are derived from one instance of this.
struct SpawnEnvSnapshot {
    http_proxy: Option<String>,
    https_proxy: Option<String>,
}

// ===================== Process lifecycle / supervision =====================
/// #954 defect 2 — outcome of the takeover connect probe. `running` rows
/// (and other adoptable non-starting shapes) keep the single-attempt
/// fast-fail: a running daemon whose socket doesn't answer is broken, so
/// boot must not stall for it. `starting` rows get the bounded readiness
/// window poll ([`SharedCodexAppServer::poll_adopt_initialized`]).
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

    /// **Invariant (#863 review R2-1)**: caller MUST hold `transition_serial`
    /// (the takeover transition runs through `start_new_process_typestate`,
    /// which assumes the guard is already held).
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
        // #953 review D3 — this settings read is the one fallible await in
        // the takeover probe that is NOT already terminalized downstream;
        // letting its error escape through `start_body_locked` /
        // `transition_replace` would release the serial with the in-memory
        // state stranded `Restarting` (no Failed state, no heal loop).
        // Terminalize like the record-read failure arm: in-memory Failed +
        // heal armed, DB row deliberately untouched — it still truthfully
        // names a live VERIFIED daemon, so writing `failed` + retained
        // identity would make the next heal round REAP a healthy survivor,
        // and NULLing identity would forge proof of absence. The next heal
        // round re-reads settings and retries the takeover.
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
        // #954 defect 3 — signature mismatch + verified healthy daemon ⇒
        // ADOPT-AND-DRAIN, never reap-for-respawn. The v2 salt (#863)
        // guarantees a mismatch on every first boot after an upgrade, so
        // the old policy executed a healthy daemon at boot — that reap is
        // what killed prod on 7/12. Adoption proceeds normally below;
        // `mark_needs_respawn` after the re-stamp drains the daemon at the
        // next thread-start boundary.
        //
        // Security property, stated precisely: adopt-and-drain guarantees
        // that no NEW thread — and therefore no new MCP child or
        // exec-shell — is ever created under detected-stale spawn
        // settings: every production mint path crosses the needs_respawn
        // drain boundary before minting (`thread_start_mint_inner` for
        // card/planner/MCP-shell mints, `ensure_respawn_for_current_settings`
        // for PTY/TUI creation). Documented residual: `turn_start` on an
        // EXISTING thread does not cross the boundary — existing threads
        // keep the MCP processes/config established under the old env
        // until the drain completes (lazily, at the next mint) or the
        // daemon is otherwise replaced. Two facts bound this residual: on
        // the upgrade-salt mismatch the actual env values are typically
        // byte-identical (only the salt changed, exposure zero); and the
        // signature hashes ONLY the salt + ingest_url + HTTP(S)_PROXY —
        // credentials and the rest of SPAWN_ENV_PASSTHROUGH were never
        // detectable by this fence under either policy.
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
        // #954 defect 2 — branch the probe on the persisted state: a
        // `starting` row names a child still inside its own cold-start
        // budget (mid-backfill after a calm-server crash/shutdown), so it
        // gets the REMAINING readiness window instead of the instant
        // handshake fast-fail that reaped it — and restarted backfill from
        // scratch — on every boot.
        let probe = if matches!(
            SharedDaemonState::from_db_str(&record.state),
            SharedDaemonState::Starting
        ) {
            // ms-domain saturating math anchored to the PERSISTED
            // `started_at`: clock skew degenerates to a full or zero
            // window, both bounded. Repeated boots against the same child
            // never re-arm the window (adoption/re-stamp preserves
            // `started_at`); a genuinely new spawn writes a new
            // `started_at` and correctly gets a fresh window.
            let elapsed = Duration::from_millis(now_ms().saturating_sub(started_at).max(0) as u64);
            let remaining = self.start_timeout.saturating_sub(elapsed);
            self.poll_adopt_initialized(&sock, pid, start_time, &boot_id, remaining)
                .await
        } else {
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
                self.install_client(notifications).await;
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
                // #953 — takeover success re-stamps `running` with the
                // adopted tuple, so a row persisted as `starting` (or another
                // legacy adoptable shape) reflects the live adopted daemon.
                //
                // #954 INVARIANT — the re-stamp keeps the OLD persisted
                // signature (`record.daemon_env_signature`); the adoption
                // path MUST NOT write the current signature anywhere. Drain
                // durability rests on nothing else: `needs_respawn` is
                // in-memory and lost on restart, so the next boot re-detects
                // the mismatch from the untouched signature, re-adopts, and
                // re-marks. On re-stamp Err the warning below stays correct
                // because the failing write is the ONLY write on this path —
                // the pre-existing row (old state, old signature) is
                // untouched and the mismatch remains durably detectable.
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
                    // #954 defect 3 — arm the drain AFTER the adoption is
                    // installed (re-stamp Ok or Err alike: the current
                    // process must drain either way, and durability comes
                    // from the untouched old signature, not this flag).
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
                // The same wall-clock deadline the child's own supervisor
                // would have enforced (#949), just by a successor process —
                // graceful (defect-1 helper), never the instant SIGKILL
                // that armed codex's backfill lease on 7/12.
                reap_verified_process_group(pid, pgid, start_time, &boot_id, self.stop_grace).await;
                Ok(false)
            }
        }
    }

    /// #953 §1 — THE atomic replace transition. Every process transition
    /// funnels through here (settings respawn, crash restart, heal loop)
    /// under a single continuously-held `transition_serial` guard: validate
    /// precondition → capture RunningProcessParts + set Restarting → abort
    /// watcher → reap → guard verify → record reconciliation →
    /// takeover-probe/spawn (typestate). The serial is released only after
    /// the terminal Running/Failed write; `resume_cached_threads` runs after
    /// release (not a transition). The precondition is validated BEFORE any
    /// destructive work, atomically with the capture, and the guard is never
    /// released in between — no second pre-spawn check needed.
    ///
    /// #954 serial-hold analysis (grace runs INSIDE `transition_serial` —
    /// required by the #953 atomic-transition invariant): a steady-state
    /// transition holds the serial for up to `stop_grace + start_timeout` ≈
    /// 60 + 120 = 180s at defaults (wedged old daemon paying the full
    /// grace, then a slow cold start). The boot takeover of a `starting`
    /// row can additionally pay the remaining readiness window before
    /// reaping, so its worst case is `window (≤ start_timeout) +
    /// stop_grace + start_timeout` ≈ 120 + 60 + 120 = 300s at defaults
    /// (unbound child consuming the full window, then the full grace, then
    /// a slow cold start). Queued behind it: thread-start
    /// settings drains, crash restarts, heal rounds (their backoff happens
    /// before acquiring), boot; track preflights observe non-Running and
    /// fail fast rather than queue. All of those already tolerate the 120s
    /// cold start; settings PUT / status snapshots never take the serial.
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
                        // #953 review D2 — generation equality alone is not
                        // staleness proof: the generation bumps only when a
                        // Running incarnation is INSTALLED, so an
                        // intervening transition that FAILED (settings
                        // respawn or heal round that consumed the crashed
                        // process and then failed to spawn) leaves it
                        // unchanged. The crashed incarnation this caller
                        // observed is "still installed" only while the
                        // state is Running (the watcher leaves the exited
                        // child's Running state in place) AND carries that
                        // generation; anything else — Failed/Restarting/
                        // Starting installed by someone else — means
                        // another transition already consumed it: abort
                        // silently — no reap, no spawn, no crash-lane
                        // retry overriding the later failure's
                        // classification (also kills the double-spawn
                        // interleaving of the old split path).
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
            // #953 PR2 review D1(b) — transition ENTRY: the state just left
            // Running (Restarting is installed), so invalidate readiness NOW
            // with the outgoing generation. Claim-boundary consumers
            // (deferred harness recovery) would otherwise still see the last
            // terminal `running: true` for the whole transition and accept a
            // transitional daemon. No premature `running: true` is possible:
            // both the Ok arm (installed Running) and every Err arm
            // (terminal Failed / in-memory terminalization) re-publish the
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

    /// #953 §3 — the ONE shared start body: guard verify → persisted-record
    /// reconciliation → takeover probe → spawn. **Invariant** (#954:
    /// type-enforced): the caller passes its `transition_serial` guard as
    /// the by-value `serial` token; every terminal Running/Failed write
    /// happens before the token drops. The takeover path drops it on
    /// return (after the terminal re-stamp); the spawn path moves it into
    /// the detached transition task.
    async fn start_body_locked(
        self: &Arc<Self>,
        serial: tokio::sync::OwnedMutexGuard<()>,
        increment_restart_count: bool,
        last_error: Option<String>,
    ) -> Result<StartedVia> {
        // #863 boot guard — the resolved home is exactly what resumed
        // threads will run against. Refuse before touching the process, and
        // never strand a previously-live polluted daemon running
        // unsupervised.
        if let Err(guard_err) = self.home.verify_expected_mcp_servers(EXPECTED_MCP_SERVERS) {
            let msg = format!("refusing to launch shared codex app-server: {guard_err}");
            self.record_guard_refusal(&msg).await;
            return Err(CalmError::CodexAppServer(msg));
        }
        // #953 §3 read-side rule (survives calm-server restarts): a `failed`
        // row with ANY identity column present is an unreconciled possible
        // survivor — run the per-shape resolution algorithm BEFORE any
        // spawn; the spawn path is unreachable until it proves absence.
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

    /// #953 §3 — boot-guard refusal arm (home pollution). Runs the same
    /// per-shape reconciliation as the failed-row path: a verified owned
    /// survivor is reaped (existing "no polluted survivor" posture, #863),
    /// anything unprovable stays unreconciled with the identity tuple
    /// RETAINED as the durable marker.
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

    /// #953 §3 — exhaustive per-shape resolution over every persisted
    /// identity combination (pid × pgid × verification-pair presence).
    /// Proof of absence ⇒ the caller may NULL identity and reopen the spawn
    /// path; anything else stays unreconciled and fail-closed. Destructive
    /// action (a verified group reap) is allowed ONLY in the full-tuple
    /// pgid==pid shape — never signal a pgid we did not create, never signal
    /// a bare pid (not a group reap; ownership may be unprovable).
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
            // Shape (b): identity fragments without a pid name no process at
            // all — nothing is establishable from the record; operator
            // remediation only (clear the identity columns).
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
            // Shape (a): pid without a complete verification pair —
            // ownership unprovable (the pid may be recycled), so never reap
            // or signal; `/proc` existence (zombies INCLUDED — #953 review
            // D4(b): a zombie leader proves nothing about its group) is the
            // only safe probe.
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
            // Natural death (or the identity matches no live process). #953
            // review D4(a) — a verified-but-ZOMBIE leader does NOT prove
            // absence here: it can no longer serve, but live descendants
            // may remain in its process group. Zombie leaders fall through
            // to the pgid arms below: the owned pgid==pid shape still runs
            // the group reap (verification permits signaling the known
            // pgid), and the unsignalable shapes stay unreconciled until
            // the leader is reaped by its parent (verify then fails).
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
                // Triple complete but no pgid: there is no valid pgid to
                // target, and signaling the bare pid is not a group reap —
                // it risks orphaning grandchildren mid-tree. Stay consistent
                // with the reap-by-pgid posture; self-resolves on death.
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
        // #953 §5 — readiness stamped on every terminal Failed.
        self.readiness.send_replace(DaemonReadiness {
            generation,
            running: false,
        });
        self.schedule_heal();
    }

    /// #953 §2 — the single Failed DB writer, with post-Ok dedup. Persist
    /// errors are `tracing::error!`-logged, never swallowed into `let _ =`,
    /// and never mask the original failure (the caller keeps its error).
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

    /// Shared spawn-transition body — #954: DETACHED from its caller.
    ///
    /// The spawn path contains unbounded awaits (the persist DB write, the
    /// up-to-`start_timeout` readiness poll), so caller cancellation inside
    /// it could previously produce a TERM'd child with no row, or a
    /// serial-released double-transition window. Instead, the ENTIRE
    /// typestate unit `start_new_process_typestate(launch_spawned_process)`
    /// — Starting flip, spawn/guard, persist, readiness poll, Running
    /// install + watcher + generation + readiness publish, and both
    /// error-terminalization arms — runs to completion inside ONE
    /// `tokio::spawn` task that owns the transition serial (the moved
    /// `serial` token) for its whole duration. Nothing is handed back: the
    /// task sends only the final `Result<()>` over a oneshot — a plain
    /// value whose drop carries no obligations. The caller merely OBSERVES:
    ///   * caller cancelled mid-transition ⇒ the task still finishes the
    ///     transition to a terminal state (Running installed, or Failed +
    ///     heal armed) — the typestate arms ARE the terminalization;
    ///   * caller cancelled after the result was sent ⇒ nothing pending,
    ///     the state is already terminal.
    ///
    /// ORDERING INVARIANT (pinned by
    /// `spawn_transition_task_outlives_caller_cancelled_at_first_await`):
    /// `tokio::spawn` is synchronous — the transition task is created (and
    /// the serial token moved into it) BEFORE this function's first await
    /// of the receiver, so a caller cancelled at that first await still
    /// leaves a running, serial-owning transition task.
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
            // The terminal Running/Failed write, readiness publish, and
            // heal arming all completed inside the typestate above; release
            // the serial BEFORE publishing the result so an observer of the
            // result never races a still-held serial.
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
            // The sender dropped without sending: the detached task
            // panicked or the runtime is tearing down (its abort → local
            // drop → SpawnedChildGuard belt SIGTERM). This observer CANNOT
            // terminalize (the task owns the serial and the typestate), so
            // the shape may be stranded: in-memory `Starting`, heal
            // unarmed, child TERM'd by the belt (#954 review D2 — the
            // accepted belt-contract residual). It is recoverable, not
            // lost: the durable row still names the child, so the next
            // transition (mint / settings drain / heal nudge) or the next
            // boot's reconciliation/adoption resolves it. Surface the
            // observation with the D2 marker so callers log it honestly.
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
        // #863 boot guard on EVERY spawn (crash-restart/respawn included):
        // pollution written while running is caught at the next launch.
        // Accepted residual (#863 review F6): the ConfigLock taken inside
        // `verify_expected_mcp_servers` is released between this verification
        // and the exec below, so a racing writer could re-pollute config.toml
        // (or drop a fresh `.env`) in that window. Accepted because all
        // legitimate writers run earlier in boot wiring (seed → sanitize →
        // ensure_*), and the guard re-runs on every respawn.
        self.home
            .verify_expected_mcp_servers(EXPECTED_MCP_SERVERS)
            .map_err(|e| {
                CalmError::CodexAppServer(format!(
                    "refusing to launch shared codex app-server: {e}"
                ))
            })?;
        // #863 review F4 — ONE settings read per spawn: the child env below
        // and the persisted signature both derive from this snapshot.
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
        // #954 — deliberately NO `.kill_on_drop(true)`: with it, tokio
        // SIGKILLs the child on every `Child` drop regardless of the
        // guard's Drop body (cancellation, panic, runtime teardown), and
        // that instant SIGKILL is exactly the lease-armer that livelocked
        // production on 7/12. Termination happens ONLY via
        // `terminate_group_with_grace` / `SpawnedChildGuard::reap_graceful`
        // / the TERM-only Drop belt.
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

        // #954 — the fallible tail: every ordinary error path (persist
        // failure, cold-start-deadline miss, Running-row write failure)
        // awaits a GRACEFUL reap of the guarded child — SIGTERM →
        // exit-driven wait up to the full stop grace → final SIGKILL —
        // while still holding the transition serial, then propagates the
        // error to the typestate choke point. The cold-start-deadline miss
        // (the mid-backfill child whose SIGKILL armed the production
        // livelock) is such an ordinary error path and gets the full grace
        // — a tiered shorter grace for the never-initialized child was
        // rejected: that tier IS the production failure.
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
        // #954 review D3 — this spawn just persisted a Running row whose
        // `daemon_env_signature` derives from the live settings snapshot.
        // If the effective signature STILL matches it, any pending
        // `needs_respawn` drain is already satisfied by this very process
        // (adopt-and-drain marked, then a crash-lane/heal-lane respawn
        // landed with current settings) — clear the flag so the next mint
        // doesn't pay a fully redundant graceful replace (cold start,
        // potentially minutes). The RE-READ (not an assumption) preserves
        // marks from settings changes that landed after this spawn's
        // snapshot: those recompute to a DIFFERENT signature and the mark
        // survives (pinned by `concurrent_mark_during_respawn_is_preserved`).
        // Documented residual: a proxy upsert + mark landing entirely
        // between this re-read and the swap below can be cleared — the
        // daemon then runs with the old proxy, but the persisted OLD
        // signature makes the next boot re-detect the mismatch and re-arm
        // the drain (adopt-and-drain), so the loss is bounded, not silent.
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
        let child = spawn_guard.disarm();
        let client = Arc::new(client);
        self.install_client(notifications).await;
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

    /// #954 — both arms go through `terminate_group_with_grace`: SIGTERM →
    /// exit-driven wait up to the stop grace (a cooperative daemon exits in
    /// its own time, never paying the full grace) → straggler cleanup
    /// (owned-child arm: group SIGKILL under the held-zombie pin;
    /// runtime-identity arm: group SIGKILL only if still alive at the
    /// deadline, else the per-member identity sweep — #954 review r2 D1).
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

    /// Wait for the freshly spawned child to bind its socket and answer
    /// `initialize`. #949 — the wait is child-liveness-aware: a missing or
    /// unready socket is NOT a failure while the child is alive (codex may
    /// spend minutes backfilling its state db before it binds), a dead child
    /// fails immediately with its exit status, and only the configured
    /// cold-start deadline reaps a live-but-never-binding child (the
    /// caller's explicit `SpawnedChildGuard::reap_graceful` does the
    /// actual reaping — #954: SIGTERM, exit-driven grace, final SIGKILL). The
    /// deadline is a TOTAL cap: it also cuts short an in-flight
    /// `connect_initialized` attempt (socket accepted, `initialize` never
    /// answered), so the configured wait cannot be exceeded by the attempt's
    /// internal 10s request timeout.
    async fn poll_connect_initialized(
        &self,
        spawn_guard: &mut SpawnedChildGuard,
    ) -> Result<(CodexAppServer, crate::codex_appserver::NotificationStream)> {
        let started = tokio::time::Instant::now();
        let deadline = started + self.start_timeout;
        loop {
            // The deadline caps the TOTAL wait, including an in-flight
            // attempt: `connect_initialized` gives `initialize` its own 10s
            // request timeout, so without `timeout_at` a child that accepts
            // the socket but never answers would stretch the configured
            // deadline by up to 10s per attempt.
            let (deadline_hit_in_flight, attempt_err) =
                match tokio::time::timeout_at(deadline, connect_initialized(&self.sock)).await {
                    Ok(Ok(pair)) => return Ok(pair),
                    Ok(Err(e)) => (false, e.to_string()),
                    Err(_) => (
                        true,
                        "initialize attempt still in flight at the cold-start deadline".to_string(),
                    ),
                };
            // Probe liveness AFTER the (possibly long) attempt so both error
            // paths below describe the child's state at emission time — a
            // child that exited during a hung attempt surfaces its exit
            // status here rather than a stale "still alive" claim.
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

    /// #954 defect 2 — bounded readiness window for a verified `starting`
    /// child: [`Self::poll_connect_initialized`]'s #949 loop with
    /// child-liveness adapted to a non-child target — `verify_owned_pid`
    /// polling instead of `spawn_guard.try_wait_exit()` (the child belongs
    /// to a previous calm-server incarnation, so no `Child` handle exists).
    /// `window` is the REMAINING budget
    /// `start_timeout − (now − persisted started_at)`; a zero window
    /// degenerates to a single raced attempt that the already-expired
    /// deadline cuts short at its first `Pending` — in practice even an
    /// already-bound socket cannot complete connect+initialize in that one
    /// poll, so the probe lapses (→ graceful reap) rather than adopting.
    /// The deadline is a TOTAL cap: it also cuts
    /// short an in-flight `connect_initialized` attempt, so the window
    /// cannot be exceeded by the attempt's internal 10s request timeout.
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
            // Liveness AFTER the (possibly long) attempt, mirroring
            // `poll_connect_initialized`: a child that exited during a
            // hung attempt is classified as exited, not lapsed. Zombie =
            // dead (#953 review D4 posture, same as
            // `terminate_group_with_grace`): an exited-but-unreaped child
            // can never bind the socket, so polling it out to the window
            // deadline would stall boot for nothing. Honest residual (the
            // r5.1 accepted class): this ChildExited classification sends
            // no signals at all — unlike the r5.1 non-owned dead-leader
            // arms it performs no per-member straggler sweep, so members
            // of the zombie leader's group can leak past the fresh spawn;
            // only a socket-holding straggler is caught by the pre-spawn
            // `reap_listener_if_alive`.
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

    /// Take an exited child out of the installed Running state, together
    /// with the generation of that Running incarnation (#953 §1: captured
    /// atomically under the core lock, so the crash restart can pass
    /// `GenerationIs` and abort if anything else replaced the process).
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
                // #953 §1 — the restart runs as its OWN task: the replace
                // transition aborts the captured watcher handle (this task),
                // so an inline restart would abort itself mid-transition.
                // Staleness is handled by the `GenerationIs` precondition,
                // not by task cancellation.
                tokio::spawn(Arc::clone(&this).restart_after_crash(error, generation));
                // #480 PR5b: restart_after_crash spawns a new SupervisorWatcher
                // for the replacement Running state. This task's loop must end
                // here so we don't accumulate one stale watcher per crash.
                // Mirrors `watch_taken_over_pid`'s return-after-restart shape.
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
                // #953 §1 — confirm the process we watched is still the
                // installed Running incarnation and capture its generation
                // atomically; if another transition already replaced it, do
                // nothing (its watcher was aborted logically — we are stale).
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
                // Own task, not inline: see `watch_spawned_child` (#953 §1 —
                // the transition aborts this watcher's handle).
                tokio::spawn(Arc::clone(&this).restart_after_crash(error, generation));
                return;
            }
        }
    }

    /// Crash-watcher restart (#953 §1). The backoff sleep happens BEFORE
    /// acquiring the transition serial — never while holding it — and the
    /// `GenerationIs` precondition aborts silently (no reap, no spawn) if
    /// any other transition already replaced the process this watcher
    /// observed dying. On a failed respawn the Failed row + heal loop are
    /// handled at the typestate choke point — the old swallowed `let _ =`
    /// crash-path persist is gone (one writer).
    ///
    /// Boxed future: the watcher → restart → transition → spawn → watcher
    /// cycle of opaque `async fn` types otherwise defeats `Send` inference.
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
                // #954 review D2 — the observation-lost error is the one
                // Err for which "terminalized + heal armed" would be a
                // false claim: the detached task died without publishing,
                // so the in-memory state may remain `Starting` with heal
                // unarmed. Recovery is the next transition (mint / settings
                // drain / heal nudge) or boot reconciliation from the
                // durable row — say that, not "recorded and armed".
                Err(e) if detached_spawn_result_lost(&e) => {
                    tracing::warn!(
                        target = "shared_codex_daemon::restart",
                        error = %e,
                        "crash restart outcome unobserved (detached spawn task panic/teardown); \
                         state unknown — recovery via the next transition or boot reconciliation"
                    );
                }
                Err(e) => {
                    // #953 review D3 — "recorded", not "persisted": every
                    // escape path terminalizes the in-memory state and arms
                    // heal, but the DB write only happens where a Failed row
                    // is trustworthy (e.g. NOT on a settings-read failure
                    // over a live takeover-eligible daemon).
                    tracing::warn!(
                        target = "shared_codex_daemon::restart",
                        error = %e,
                        "crash restart failed; terminal failed state recorded and heal loop armed"
                    );
                }
            }
        })
    }

    /// #480 §C — typestate transition: begin/finish a fresh process spawn.
    /// **Invariant**: caller MUST hold `transition_serial` for the duration
    /// (#863 review R2-1: acquisition moved to the callers so the boot path
    /// can hold ONE guard across boot-guard + takeover/spawn without
    /// re-locking here). Callers: `try_takeover_live` and
    /// `spawn_process_transition`, both under a caller-held guard.
    ///
    /// #953 §1 — this is THE Failed-persistence choke point: any spawn
    /// failure (pre- or post-`persist_runtime_starting`) persists a `failed`
    /// row here with the identity tuple NULLed — NULL is legitimate because
    /// absence is proven for both cases (a self-spawned child is our direct
    /// child, reaped by `SpawnedChildGuard`; a pre-spawn failure never had a
    /// child). The unreconciled boot-guard case never reaches this arm — it
    /// errors before `spawn()` and RETAINS identity (§3).
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
                    // #953 §1 — a new Running incarnation is installed: bump
                    // the generation (wrapping; equality-only consumers).
                    core.generation = core.generation.wrapping_add(1);
                    core.state = SupervisorState::Running {
                        child: launched.child,
                        client: launched.client,
                        runtime: launched.runtime,
                        watcher: launched.watcher,
                    };
                    core.generation
                };
                // #953 §5 — readiness stamped on every installed Running.
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
                // #953 §5 — readiness stamped on every terminal Failed.
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

// ===================== #953 §2 — self-heal loop =====================
impl SharedCodexAppServer {
    /// Arm the background heal loop: at most one instance via CAS on
    /// `heal_active`; the claim is released by [`HealActiveGuard`]'s RAII
    /// `Drop` on panic, cancellation, abort, and normal exit alike. No-op in
    /// fixtures fake mode (stubs never reach spawn paths). Returns the task
    /// handle (tests use it; production callers drop it — the task holds
    /// only a `Weak` and exits when the supervisor drops).
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

    /// Infinite classified retry — no give-up-after-N (give-up is the
    /// defect-3 lockout reborn; systemd `Restart=always` precedent). Each
    /// round: sleep by class (select against the settings-change nudge for
    /// the slow lane's immediate wake) → upgrade the Weak → `ensure_running`.
    /// Ok ⇒ release the claim, re-check the terminal state (re-arming on
    /// Failed — #953 review D1), and exit. Err ⇒ reclassify from
    /// the fresh Failed state and continue. Unreconciled rounds run only
    /// guard + reconciliation — `ensure_running`'s body keeps the spawn path
    /// unreachable until absence is proven.
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
                    // #953 review D1 fixtures gate — see the field doc: a
                    // test holding this mutex parks the healed task here,
                    // keeping the claim held across a concurrent failure.
                    #[cfg(feature = "fixtures")]
                    drop(strong.heal_post_ok_gate.lock().await);
                    // #953 review D1 — release the singleton claim BEFORE
                    // the terminal re-check. `ensure_running()` Ok is not
                    // proof the daemon is still Running: the transition
                    // serial was released before thread resume, so a crash
                    // + failed restart may already have installed Failed —
                    // and ITS `schedule_heal()` CAS silently lost against
                    // the claim this task was still holding. Releasing
                    // first, then re-reading, closes BOTH orderings:
                    //  * failure's schedule_heal ran BEFORE this release:
                    //    every failure path flips the state to Failed
                    //    before calling schedule_heal, so the re-check
                    //    below observes Failed and re-arms the loop;
                    //  * failure's schedule_heal runs AFTER this release:
                    //    its CAS wins normally (and a concurrent re-arm
                    //    below dedups on the same CAS).
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

    /// Delay for the next heal round: fast lane = the existing
    /// `BackoffState` knobs; slow lane = same exponential/attempts with
    /// floor `restart_max_delay_ms` and ceiling [`HEAL_SLOW_RETRY_CEILING`];
    /// ±20% jitter on every delay. Backoff reset keeps the existing
    /// `reset_if_stable` semantics — brief Running does NOT reset attempts.
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

// ===================== Notification routing & thread cache =====================
impl SharedCodexAppServer {
    async fn install_client(&self, mut notifications: crate::codex_appserver::NotificationStream) {
        // #741 §1.3 — stamp the daemon (re)connect wall-clock. This is the
        // common path for BOTH fresh-spawn and hot-takeover connects, so the
        // 741-3 reaper's REBUILD_GRACE is reset on every reconnect. Always-on
        // (cheap); nothing consumes it until 741-3.
        self.daemon_connected_at_ms
            .store(now_ms(), Ordering::SeqCst);
        let tx = self.notifications.clone();
        let pending = self.pending_codex_threads_handle.clone();
        let repo = self.repo.clone();
        let thread_cache = self.thread_cache.clone();
        let active_turns = self.active_turns.clone();
        let kernel_initiated_threads = self.kernel_initiated_threads.clone();
        let kernel_thread_start_serial = self.kernel_thread_start_serial.clone();
        tokio::spawn(async move {
            while let Some(notification) = notifications.recv().await {
                if let Some(thread_id) = thread_started_id(&notification) {
                    match handle_thread_started_notification(
                        pending.as_ref(),
                        &repo,
                        &thread_cache,
                        &kernel_initiated_threads,
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
                track_active_turn(&active_turns, &notification);
                if let Some(thread_id) = turn_completed_thread_id(&notification) {
                    kernel_initiated_threads.lock().await.remove(thread_id);
                }
                let _ = tx.send(notification);
            }
        });
    }

    async fn rebuild_thread_cache_from_db(&self) -> Result<()> {
        self.thread_cache.clear();
        self.active_turns.clear();

        let active_threads = merge_active_shared_thread_attribution(self.repo.as_ref()).await?;
        for (card_id, thread_id) in active_threads {
            self.thread_cache.insert(thread_id, card_id);
        }
        Ok(())
    }

    async fn resume_cached_threads(&self, mode: ResumeMode) {
        let Some(client) = self.running_client().await else {
            return;
        };
        for entry in self.thread_cache.iter() {
            let thread_id = entry.key().clone();
            let card_id = entry.value().clone();
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

            let raw_token = match write_in_tx_typed(self.repo.as_ref(), {
                let thread_id = thread_id.clone();
                let card_id = card_id.clone();
                move |tx| {
                    Box::pin(async move {
                        let Some(runtime) =
                            session_projection_active_for_card_tx(tx, &card_id).await?
                        else {
                            return Ok(None);
                        };
                        if runtime.thread_id.as_deref() != Some(thread_id.as_str()) {
                            return Ok(None);
                        }
                        mint_and_persist_card_token(tx, &card_id, &runtime.id)
                            .await
                            .map(Some)
                    })
                }
            })
            .await
            {
                Ok(Some(raw_token)) => raw_token,
                Ok(None) => {
                    Self::resume_thread_typed(&client, &thread_id, &card_id, ThreadConfig::NoMcp)
                        .await;
                    continue;
                }
                Err(e) => {
                    Self::resume_thread_typed(&client, &thread_id, &card_id, ThreadConfig::NoMcp)
                        .await;
                    tracing::warn!(
                        target = "shared_codex_daemon::resume",
                        %thread_id,
                        %card_id,
                        error = %e,
                        "shared codex thread token refresh failed; resumed without config"
                    );
                    continue;
                }
            };
            // Invariant: only the cold respawn caller may rotate and reemit
            // per-card MCP config, and only for the card's active thread.
            // Hot takeover always plain-resumes because loaded threads ignore
            // resume config and keep using their existing environment.
            Self::resume_thread_typed(
                &client,
                &thread_id,
                &card_id,
                ThreadConfig::McpShell {
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
        let lowered = config.to_wire_config();
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

// ===================== Test-only helpers =====================
#[cfg(any(test, feature = "fixtures"))]
impl SharedCodexAppServer {
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
    pub fn fail_next_thread_start_for_test(&self) {
        if let Some(fake) = self.fake.as_ref() {
            fake.fail_next_thread_start.store(true, Ordering::SeqCst);
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

    /// #953 — whether the singleton heal-loop claim is currently held.
    pub fn heal_active_for_test(&self) -> bool {
        self.heal_active.load(Ordering::SeqCst)
    }

    /// #953 — arm the heal loop directly (same CAS/RAII path as production).
    pub fn schedule_heal_for_test(self: &Arc<Self>) -> Option<JoinHandle<()>> {
        self.schedule_heal()
    }

    /// #953 review D1 — the heal loop's post-Ok interleaving gate (fixtures
    /// builds only): a test that locks this mutex parks the heal task AFTER
    /// a successful round installed Running but BEFORE the task releases the
    /// singleton `heal_active` claim.
    #[cfg(feature = "fixtures")]
    pub fn heal_post_ok_gate_for_test(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.heal_post_ok_gate)
    }

    /// #953 PR2 review D1 — the transition-entry interleaving gate (fixtures
    /// builds only): a test that locks this mutex parks a
    /// `transition_replace` AFTER the state left Running (readiness
    /// invalidated with the outgoing generation) but BEFORE the reap/start
    /// body runs.
    #[cfg(feature = "fixtures")]
    pub fn transition_entry_gate_for_test(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.transition_entry_gate)
    }

    /// #953 — current Running-incarnation generation.
    pub async fn generation_for_test(&self) -> u64 {
        self.core.lock().await.generation
    }

    /// #954 T7 — abort the most recent DETACHED spawn transition task
    /// (models a runtime-teardown abort / in-task panic; caller
    /// cancellation can no longer reach the transition). Returns whether a
    /// task handle was present to abort.
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

    /// #954 — acquire the transition serial as the owned by-value token the
    /// `_locked` chain threads (for the spawn-ordering pin test).
    #[cfg(feature = "fixtures")]
    pub async fn lock_transition_serial_for_test(&self) -> tokio::sync::OwnedMutexGuard<()> {
        Arc::clone(&self.transition_serial).lock_owned().await
    }

    /// #954 — the raw detached spawn transition as an un-polled future, so
    /// the ordering pin test can poll it exactly once (creating the
    /// detached task) and then DROP it (caller cancelled at its first
    /// await).
    #[cfg(feature = "fixtures")]
    pub fn detached_spawn_transition_future_for_test(
        self: &Arc<Self>,
        serial: tokio::sync::OwnedMutexGuard<()>,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static {
        let this = Arc::clone(self);
        async move { this.spawn_process_transition(serial, false, None).await }
    }

    /// #953 — drive the atomic replace transition with an explicit
    /// precondition (the crash watcher's production path).
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

/// #954 — where the termination signals go.
enum SignalScope {
    /// Signal the whole process group (the normal spawn-invariant target).
    Group { pgid: i32 },
    /// Signal a bare pid — ONLY for `reap_listener_if_alive`'s
    /// getpgid-failure fallback, where no valid pgid is derivable.
    Pid { pid: i32 },
}

impl SignalScope {
    fn send(&self, signal: i32) {
        match *self {
            SignalScope::Group { pgid } => {
                signal_process_group(pgid, signal);
            }
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
            SignalScope::Pid { pid } => ("pid", pid),
        }
    }
}

/// #954 — how the helper observes the LEADER's exit.
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
    ProcPresence { pid: i32 },
}

/// #954 review D1 (r2-hardened) — how the leader's exit was observed,
/// which decides what straggler cleanup is safe. Once a group's last
/// member is reaped the kernel may recycle the numeric pgid, so an
/// unverified `kill(-pgid, SIGKILL)` could hit an unrelated group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaderExit {
    /// Exit observed with the leader still an UNREAPED zombie. Whether
    /// that zombie PINS the group across the observation→signal interval
    /// depends on who reaps it (#954 review r2 D1):
    ///   * Owned `Child`: WE hold the unreaped zombie (`child.wait()` is
    ///     deferred until after the signal decision), so the pin is
    ///     guaranteed — the final group SIGKILL is safe and is sent for
    ///     straggler descendants.
    ///   * Non-owned (`VerifiedIdentity` / `ProcPresence`): the EXTERNAL
    ///     parent can reap the zombie at any moment after our
    ///     observation, un-pinning the pgid — so a group-wide signal
    ///     still races recycling (the r1 shape merely narrowed the race
    ///     to ε). Treated like [`LeaderExit::FullyGone`]: no group-wide
    ///     signal; remaining members are swept individually under
    ///     per-pid identity re-verification instead.
    ExitedPinned,
    /// The leader was fully reaped (`/proc` entry gone / identity
    /// mismatch, or an owned child already waited on): the group may be
    /// empty and its numeric id recycled — no group-wide signal is ever
    /// sent. Non-owned paths sweep the remaining enumerable members
    /// individually (verify-then-signal per pid); see
    /// `terminate_group_with_grace` step 3 for the recorded residual.
    FullyGone,
    /// Grace ceiling elapsed with the leader still alive (not a zombie):
    /// the live leader is a member, so the group provably exists and
    /// cannot be recycled — group SIGKILL as before.
    AliveAtDeadline,
}

/// #954 defect 1 — THE shared exit-driven termination helper, replacing
/// every fixed post-SIGTERM sleep. Steps:
///  1. SIGTERM the scope.
///  2. Wait for the LEADER's exit up to `grace`. The grace is a CEILING,
///     not a sleep: a cooperative daemon (handles SIGTERM, checkpoints,
///     exits in <2s) pays its actual exit time (plus at most the ≤100ms
///     observation poll) — no fixed grace tax; a wedged daemon pays the
///     full grace before SIGKILL.
///  3. Straggler cleanup, gated on WHO controls the leader's zombie (#954
///     review D1, hardened in r2):
///     * Leader alive at the deadline → group SIGKILL (a live leader pins
///       the group; recycling is impossible).
///     * Owned `Child` observed exited → group SIGKILL. The Child arm
///       observes exit WITHOUT reaping — `/proc` state poll, no
///       `child.wait()` — so WE hold the unreaped zombie and it pins the
///       pgid until after the signal; the child is reaped only afterwards.
///     * Non-owned leader (`VerifiedIdentity` / `ProcPresence`) observed
///       zombie OR fully reaped → NO group-wide signal, ever: the
///       external parent can reap the zombie between our observation and
///       a `kill(-pgid, …)`, letting the kernel recycle the numeric pgid
///       (#954 review r2 D1 — observing `Z` does not preserve the pin).
///       Instead the remaining group members are enumerated
///       (`/proc/*/stat` `pgrp == pgid`) and SIGKILLed INDIVIDUALLY,
///       each under a scan-time `start_time` capture re-verified
///       immediately before its signal — the same verify-then-signal
///       posture as every `verify_owned_pid`-then-`kill` site. Zombie
///       members are skipped (already dead; their parent reaps them).
///       Recorded residual (#954 review r2 D2): the sweep covers only
///       members that are enumerable and identity-verifiable at sweep
///       time — a member that forks after the scan, or whose re-verify
///       fails, is deliberately left alone. Such a TERM-surviving
///       straggler stays discoverable through the `--listen
///       unix://<sock>` argv contract and self-limits on bind conflict
///       (the same mitigation as the guard-drop orphan).
///  4. Bounded post-wait; callers with identity keep their existing
///     `survivor_alive_after_group_reap` post-check.
async fn terminate_group_with_grace(
    scope: SignalScope,
    wait: ExitWait<'_>,
    grace: Duration,
) -> LeaderExit {
    let (scope_kind, scope_id) = scope.describe();
    // #954 review D1 — an owned child that was ALREADY reaped before this
    // helper ran (cold-start fail-fast `try_wait`) means the leader is gone
    // and the numeric pgid may be recycled: nothing here is safely
    // signalable anymore, not even the initial SIGTERM.
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
            // Observe WITHOUT reaping (#954 review D1): `child.wait()`
            // would release the zombie and un-pin the pgid before the
            // final group signal. An unreaped direct child can never leave
            // `/proc`, so state `Z` is exactly "exited, still pinning".
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
            // Post-signal poll: zombie = dead (#953 review D4 posture).
            // The Z/FullyGone distinction no longer changes the cleanup
            // (#954 review r2 D1 — a non-owned zombie's pin does not
            // survive the observation→signal interval; both arms sweep),
            // but it is kept for the caller-visible outcome and logs.
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
            // Owned Child: WE hold the unreaped zombie (reaped only after
            // this signal), so the pgid is provably pinned — the group
            // SIGKILL is safe (r2-review-verified arm).
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
            // Non-owned leader observed zombie or fully reaped (#954
            // review r2 D1): the external parent may reap the zombie —
            // or already has — between observation and any group signal,
            // and the kernel may then recycle the numeric pgid, so NO
            // group-wide signal is sent from here. Stragglers are swept
            // individually under per-pid verify-then-signal instead; see
            // the helper doc (step 3) for the recorded residual.
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
                SignalScope::Pid { .. } => {
                    // Pid-fallback scope: the only known target IS the
                    // dead leader; there is no pgid to enumerate and a
                    // bare-pid SIGKILL races recycling identically —
                    // nothing further can be signaled safely.
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

/// #954 — ≤100ms observation poll against the grace deadline, classifying
/// the leader's state (#954 review D1). Returns
/// [`LeaderExit::AliveAtDeadline`] when the ceiling elapses without an
/// exit observation.
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

/// SIGTERM → exit-driven wait up to `grace` → straggler cleanup (#954;
/// previously a fixed 500ms sleep then SIGKILL): group SIGKILL only while
/// the leader is provably alive at the deadline; a dead (zombie/reaped)
/// leader triggers the per-member identity sweep instead (#954 review r2
/// D1 — this path does not own the leader's zombie).
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

    // Post-signal check: zombie = dead (we just SIGKILLed the group; the
    // leader may linger as a zombie until its parent waits — #953 review D4).
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

fn track_active_turn(active_turns: &DashMap<String, String>, notification: &Notification) {
    match notification {
        Notification::TurnStarted { thread_id, turn } => {
            if let Some(turn_id) = turn_id(turn) {
                active_turns.insert(thread_id.clone(), turn_id.to_string());
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

    match pending.on_thread_started(thread_id).await? {
        Some(card_id) => {
            thread_cache.insert(thread_id.to_string(), card_id);
            Ok(ThreadStartedHandling::PendingBound)
        }
        None => Ok(ThreadStartedHandling::DispatchNormally),
    }
}

async fn reap_listener_if_alive(sock_path: &Path, grace: Duration) -> Result<()> {
    let Ok(stream) = UnixStream::connect(sock_path).await else {
        return Ok(());
    };

    let fd = stream.as_raw_fd();
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut cred_len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut cred_len,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!(
            target: "shared_codex_daemon::stop",
            error = %err,
            sock = %sock_path.display(),
            "SO_PEERCRED failed; proceeding to unlink listener-bound socket without reap"
        );
        return Ok(());
    }

    let peer_pid = cred.pid;
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
        // #954 — graceful pid-fallback reap (exit observed via /proc).
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
    // #954 — graceful group reap: SIGTERM → exit-driven wait (leader exit
    // observed via /proc; a post-signal zombie counts as exited) →
    // straggler cleanup (group SIGKILL only for a leader still alive at
    // the deadline; otherwise the per-member identity sweep — #954
    // review r2 D1).
    terminate_group_with_grace(
        SignalScope::Group { pgid },
        ExitWait::ProcPresence { pid: peer_pid },
        grace,
    )
    .await;
    Ok(())
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

    /// #949 — non-blocking liveness probe on the still-guarded child. The
    /// cold-start poll uses it to fail fast (with the exit status) when the
    /// child dies before its socket appears. `None` while the child runs, or
    /// on a probe error (the deadline then remains the backstop).
    fn try_wait_exit(&mut self) -> Option<std::process::ExitStatus> {
        self.spawned_child
            .as_mut()
            .and_then(|child| child.try_wait().ok())
            .flatten()
    }

    /// #954 — explicit graceful reap on the launch error path: SIGTERM →
    /// exit-driven wait up to `grace` → final group SIGKILL, consuming the
    /// guard (its Drop belt is disarmed).
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
    /// #954 — SIGTERM-only belt. With the spawn transition detached from
    /// its caller, caller cancellation can no longer reach this guard; the
    /// belt fires only on a panic inside — or a runtime-teardown abort of —
    /// the detached transition task (abort → local drop → belt TERM). No
    /// SIGKILL here (and no tokio `kill_on_drop`): the instant SIGKILL is
    /// the lease-armer #954 removes. A TERM'd codex aborts its backfill
    /// cleanly (no 900s lease) and self-exits on bind conflict; the
    /// discoverability contract for a wedged orphan is its argv
    /// (`--listen unix://<data_dir>/.../codex-appserver`), operator kills
    /// by VERIFIED pid only.
    ///
    /// #954 review D2 — this belt does NOT terminalize the supervisor: a
    /// panic in the detached task unwinds the serial and can leave
    /// in-memory `Starting` with heal unarmed (the accepted belt-contract
    /// residual). The shape is stranded, not lost — the durable row still
    /// names the child, and if the TERM'd child survives (or was already
    /// initialized), the next transition or boot reconciliation/adoption
    /// recovers it.
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

// #954 — `impl Drop for SharedCodexAppServer` is DELETED deliberately.
// INVARIANT: calm-server shutdown NEVER signals the shared codex daemon;
// the daemon is left running for the next boot's takeover (#953 re-stamp).
// The old Drop SIGTERM'd the Running daemon's pgid — it never fired in
// production while SIGTERM killed the process before drops ran, but WOULD
// fire once main.rs's graceful shutdown (#954 defect 4) returns from main,
// silently defeating takeover. Test teardown uses explicit cleanup (or the
// fixture's pdeathsig hygiene belt), never a supervisor Drop.

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

    /// Pull the #741 §1.3 liveness facts via `thread/read(include_turns)`
    /// (+ `thread/loaded/list` for the `loaded` flag). `None` on ANY RPC
    /// error / unreachable daemon — the arbiter treats that as `Unknown`.
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

/// Map the wire `thread/read` response (+ `loaded` flag) into the
/// arbiter-facing [`CodexLivenessFacts`] (#741 §1.3). The "last turn" is the
/// MOST RECENT element of `turns`; its `completedAt` is the died-mid-turn
/// discriminator (§0.1).
fn liveness_facts_from_read(
    read: crate::codex_appserver::ThreadReadResponse,
    loaded: bool,
) -> calm_provider::provider::CodexLivenessFacts {
    use crate::codex_appserver::{ThreadActiveFlag, ThreadStatus};
    use calm_provider::provider::{CodexLivenessFacts, ThreadStatusLite};

    let status = match read.thread.status {
        ThreadStatus::NotLoaded => ThreadStatusLite::NotLoaded,
        ThreadStatus::Idle => ThreadStatusLite::Idle,
        ThreadStatus::SystemError => ThreadStatusLite::SystemError,
        ThreadStatus::Active { active_flags } => ThreadStatusLite::Active {
            waiting_on_user_input: active_flags.contains(&ThreadActiveFlag::WaitingOnUserInput),
            waiting_on_approval: active_flags.contains(&ThreadActiveFlag::WaitingOnApproval),
        },
    };
    // `last_turn_completed_at`: None = no turns present (None or empty list);
    // Some(None) = last turn never finished; Some(Some(ts)) = finished/aborted.
    let last_turn_completed_at = read
        .thread
        .turns
        .as_deref()
        .and_then(|turns| turns.last())
        .map(|turn| turn.completed_at);
    CodexLivenessFacts {
        loaded,
        status,
        last_turn_completed_at,
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

    /// #953 design test 7 — the dedup cache is updated ONLY after a
    /// successful DB write (`note_written`): a forced write failure (the
    /// caller skipping `note_written`) leaves the cache unset so the next
    /// identical round retries the write instead of masking it.
    #[test]
    fn failed_persist_dedup_updates_only_after_successful_write() {
        let dedup = FailedPersistDedup::default();
        let identity = FailedIdentity::proven_absent(Path::new("/tmp/s"), Path::new("/tmp/h"));
        assert!(!dedup.should_skip("boom", FailureClass::Persistent, &identity));
        // Simulated DB-write failure: `note_written` is NOT called (that is
        // the code-level contract at the persist site) — the tuple must
        // still be written next round.
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

    /// #953 §2 — lane classification: the two cold-start poll failures and
    /// child-exit shapes are Transient; exec/config/guard failures are
    /// Persistent.
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

    /// #953 §2 — jitter stays within ±20%.
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

    /// #953 design test 7 — Persistent hits the 300s slow ceiling with a
    /// floor at `restart_max_delay`; Transient stays on the fast lane cap.
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

    /// #953 §3 — read-side classification rule over row shapes.
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

    /// #953 review D3 — a settings-read failure inside `try_takeover_live`
    /// (`current_env_signature`) during a takeover-eligible transition must
    /// not escape the choke point with the in-memory state stranded
    /// `Restarting`: the serial may only release on a TERMINAL in-memory
    /// state, with the heal loop armed. The DB row is deliberately left
    /// untouched: it still truthfully names a live verified daemon, and the
    /// next heal round re-reads settings and retries the takeover (writing
    /// `failed`+retained identity would make the next round REAP a healthy
    /// survivor; NULLing identity would forge proof of absence).
    ///
    /// Injection: drop ONLY the `settings` table — `settings_get_all` fails
    /// while `shared_daemon_runtime_get` stays healthy. The "live verified
    /// daemon" is this test process itself (verifiable identity, never
    /// signaled by this path).
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

    /// #863 — the v2 schema salt must change the signature for identical
    /// inputs, so the first post-upgrade boot mismatches every pre-upgrade
    /// persisted signature and forces exactly one normalize-respawn.
    #[test]
    fn env_signature_v2_salt_differs_from_pre_salt_signature() {
        let ingest = "http://127.0.0.1:8765";
        let mut h = Sha256::new();
        h.update(ingest.as_bytes());
        h.update(b"|");
        h.update(b"|");
        let pre_salt = hex::encode(h.finalize())[..16].to_string();

        let v2 = SharedCodexAppServer::compute_env_signature(ingest, None, None);
        assert_ne!(
            v2, pre_salt,
            "compute_env_signature must be salted (env-schema-v2:863)"
        );
    }

    /// #863 — with `env_clear()`, a parent-env proxy must be RESOLVED and
    /// set explicitly when settings are absent (the old behavior was
    /// implicit inheritance). Settings still win over the parent env.
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
