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
use std::sync::atomic::{AtomicBool, Ordering};
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
};
use crate::config::Config;
use crate::db::{Repo, SharedCodexDaemonUpdate};
use crate::error::{CalmError, Result};
use crate::model::{CardRole, now_ms};
use crate::pending_codex_threads::PendingThreadStartRegistry;
use crate::routes::settings::load_settings;
use crate::shared_codex_home::SharedCodexHome;
use crate::spec_appserver::{
    read_boot_id, read_proc_start_time, signal_process_group, verify_owned_pid,
};

pub type TurnId = String;

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

#[derive(Debug, Clone)]
pub struct SharedThreadStartParams {
    pub cwd: String,
    pub approval_policy: String,
    pub sandbox_mode: String,
    pub developer_instructions: Option<String>,
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
    state: Arc<Mutex<SharedDaemonState>>,
    sock: PathBuf,
    home: Arc<SharedCodexHome>,
    runtime: Arc<Mutex<Option<SharedDaemonRuntime>>>,
    repo: Arc<dyn Repo>,
    thread_cache: Arc<DashMap<String, String>>,
    active_turns: Arc<DashMap<String, String>>,
    restart_backoff: BackoffState,
    notifications: NotificationFanout,
    pending_codex_threads_handle: Option<Arc<PendingThreadStartRegistry>>,
    kernel_initiated_threads: Arc<Mutex<HashSet<String>>>,
    kernel_thread_start_serial: Arc<Mutex<()>>,
    codex_bin: String,
    log_dir: PathBuf,
    client: Arc<Mutex<Option<Arc<CodexAppServer>>>>,
    child: Arc<Mutex<Option<Child>>>,
    monitor_started: AtomicBool,
    restart_lock: Mutex<()>,
    restart_count: std::sync::atomic::AtomicU64,
    needs_respawn_on_next_thread_start: Arc<AtomicBool>,
    taken_over_pid_watcher: Mutex<Option<JoinHandle<()>>>,
    last_error: Arc<Mutex<Option<String>>>,
    // ----- #480 PR5a additions (parallel state — read+written by transition APIs;
    // existing fields remain authoritative until PR5b migration) -----
    /// #480 §C — typestate-companion state machine. PR5b migrates readers.
    core: Arc<tokio::sync::Mutex<SupervisorCore>>,
    /// #480 §C — serializes process transitions (replaces `restart_lock` in PR5b).
    transition_serial: Arc<tokio::sync::Mutex<()>>,
    ingest_url: String,
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
}

pub struct LaunchedSharedDaemon {
    pub child: Option<Child>,
    pub client: Arc<CodexAppServer>,
    pub runtime: SharedDaemonRuntime,
    pub watcher: SupervisorWatcher,
}

impl SharedCodexAppServer {
    pub fn new_stub(repo: Arc<dyn Repo>) -> Arc<Self> {
        Self::new_stub_inner(repo, None)
    }

    #[cfg(feature = "fixtures")]
    pub fn new_stub_with_pending(
        repo: Arc<dyn Repo>,
        pending_codex_threads_handle: Option<Arc<PendingThreadStartRegistry>>,
    ) -> Arc<Self> {
        Self::new_stub_inner(repo, pending_codex_threads_handle)
    }

    fn new_stub_inner(
        repo: Arc<dyn Repo>,
        pending_codex_threads_handle: Option<Arc<PendingThreadStartRegistry>>,
    ) -> Arc<Self> {
        let root = std::env::temp_dir().join(format!(
            "neige-shared-codex-appserver-stub-{}",
            uuid::Uuid::new_v4()
        ));
        let legacy = root.join("codex-homes");
        let home = Arc::new(SharedCodexHome::new(root.join("codex-home"), legacy));
        let (tx, _) = broadcast::channel(16);
        Arc::new(Self {
            state: Arc::new(Mutex::new(SharedDaemonState::Idle)),
            sock: root.join("run/codex-appserver.sock"),
            home,
            runtime: Arc::new(Mutex::new(None)),
            repo,
            thread_cache: Arc::new(DashMap::new()),
            active_turns: Arc::new(DashMap::new()),
            restart_backoff: BackoffState::new(Duration::from_millis(250), Duration::from_secs(10)),
            notifications: tx,
            pending_codex_threads_handle,
            kernel_initiated_threads: Arc::new(Mutex::new(HashSet::new())),
            kernel_thread_start_serial: Arc::new(Mutex::new(())),
            codex_bin: "codex".into(),
            log_dir: root.join("logs/shared-codex-appserver"),
            client: Arc::new(Mutex::new(None)),
            child: Arc::new(Mutex::new(None)),
            monitor_started: AtomicBool::new(false),
            restart_lock: Mutex::new(()),
            restart_count: std::sync::atomic::AtomicU64::new(0),
            needs_respawn_on_next_thread_start: Arc::new(AtomicBool::new(false)),
            taken_over_pid_watcher: Mutex::new(None),
            last_error: Arc::new(Mutex::new(None)),
            core: Arc::new(tokio::sync::Mutex::new(SupervisorCore {
                state: SupervisorState::Idle,
                attempts: 0,
            })),
            transition_serial: Arc::new(tokio::sync::Mutex::new(())),
            ingest_url: "http://127.0.0.1:0".into(),
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
            state: Arc::new(Mutex::new(SharedDaemonState::Idle)),
            sock: data_dir.join("run/codex-appserver.sock"),
            home,
            runtime: Arc::new(Mutex::new(None)),
            repo,
            thread_cache: Arc::new(DashMap::new()),
            active_turns: Arc::new(DashMap::new()),
            restart_backoff: BackoffState::new(
                Duration::from_millis(cfg.shared_codex_appserver_restart_initial_delay_ms),
                Duration::from_millis(cfg.shared_codex_appserver_restart_max_delay_ms),
            ),
            notifications: tx,
            pending_codex_threads_handle,
            kernel_initiated_threads: Arc::new(Mutex::new(HashSet::new())),
            kernel_thread_start_serial: Arc::new(Mutex::new(())),
            codex_bin: cfg.codex_bin.clone(),
            log_dir: cfg.shared_codex_appserver_log_dir_resolved(),
            client: Arc::new(Mutex::new(None)),
            child: Arc::new(Mutex::new(None)),
            monitor_started: AtomicBool::new(false),
            restart_lock: Mutex::new(()),
            restart_count: std::sync::atomic::AtomicU64::new(0),
            needs_respawn_on_next_thread_start: Arc::new(AtomicBool::new(false)),
            taken_over_pid_watcher: Mutex::new(None),
            last_error: Arc::new(Mutex::new(None)),
            core: Arc::new(tokio::sync::Mutex::new(SupervisorCore {
                state: SupervisorState::Idle,
                attempts: 0,
            })),
            transition_serial: Arc::new(tokio::sync::Mutex::new(())),
            ingest_url: cfg.codex_ingest_url_resolved(),
        })
    }

    pub async fn start_or_takeover(self: &Arc<Self>) -> Result<()> {
        self.rebuild_thread_cache_from_db().await?;
        let record = self.repo.shared_daemon_runtime_get().await?;
        if self.try_takeover_live(&record).await? {
            if let Some(runtime) = self.runtime.lock().await.clone() {
                self.spawn_taken_over_pid_watcher(runtime).await;
            }
            return Ok(());
        }

        self.remove_stale_socket_before_spawn().await?;
        self.start_new_process(SharedDaemonState::Starting, false, None)
            .await?;
        self.spawn_spawned_child_watcher();
        Ok(())
    }

    pub async fn thread_start_for_card(
        self: &Arc<Self>,
        card_id: &str,
        role: CardRole,
        wave_id: Option<&str>,
        params: SharedThreadStartParams,
    ) -> Result<String> {
        let _start_guard = self.kernel_thread_start_serial.lock().await;
        self.reap_and_respawn_with_current_settings().await?;
        let client = self.client().await?;
        let thread = client
            .thread_start_with_params(ThreadStartParams {
                cwd: params.cwd,
                approval_policy: params.approval_policy,
                sandbox_mode: params.sandbox_mode,
                developer_instructions: params.developer_instructions,
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
        self.repo
            .card_codex_thread_upsert(card_id, &thread_id, role, wave_id)
            .await?;
        self.thread_cache
            .insert(thread_id.clone(), card_id.to_string());
        tracing::info!(
            target = "shared_codex_daemon::thread_start",
            %card_id,
            ?role,
            thread_id = %thread_id,
            wave_id,
            "shared codex app-server thread started"
        );
        Ok(thread_id)
    }

    /// If runtime settings changed, synchronously respawn the daemon so
    /// later TUI-started `thread/start` calls hit a process with current env.
    pub async fn ensure_respawn_for_current_settings(self: &Arc<Self>) -> Result<()> {
        let _start_guard = self.kernel_thread_start_serial.lock().await;
        self.reap_and_respawn_with_current_settings().await
    }

    pub async fn turn_start(&self, thread_id: &str, items: Vec<InputItem>) -> Result<TurnId> {
        if !self.thread_cache.contains_key(thread_id) {
            tracing::warn!(
                target = "shared_codex_daemon::mapping_miss",
                %thread_id,
                method = "turn/start",
                "turn/start for thread missing shared daemon card mapping"
            );
        }
        let client = self.client().await?;
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
        let client = self.client().await?;
        client.turn_interrupt(thread_id, turn_id).await
    }

    pub async fn interrupt_active_turn(&self, thread_id: &str) -> Result<()> {
        let Some(turn_id) = self
            .active_turns
            .get(thread_id)
            .map(|entry| entry.value().clone())
        else {
            return Ok(());
        };
        self.turn_interrupt(thread_id, &turn_id).await
    }

    pub async fn interrupt_active_turn_for_card(&self, card_id: &str) -> Result<()> {
        let Some(row) = self.repo.card_codex_thread_get_by_card(card_id).await? else {
            return Ok(());
        };
        self.interrupt_active_turn(&row.thread_id).await
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }

    pub(crate) async fn thread_id_bound_to_card(&self, card_id: &str) -> Result<Option<String>> {
        Ok(self
            .repo
            .card_codex_thread_get_by_card(card_id)
            .await?
            .map(|row| row.thread_id))
    }

    pub fn is_running(&self) -> bool {
        self.state
            .try_lock()
            .is_ok_and(|state| *state == SharedDaemonState::Running)
    }

    pub fn remote_uri(&self) -> String {
        format!("unix://{}", self.sock.display())
    }

    pub fn mark_needs_respawn(&self) {
        self.needs_respawn_on_next_thread_start
            .store(true, Ordering::SeqCst);
    }

    pub fn status_snapshot(&self) -> SharedDaemonStatus {
        SharedDaemonStatus {
            state: self
                .state
                .try_lock()
                .map(|g| *g)
                .unwrap_or(SharedDaemonState::Failed),
            sock: self.sock.display().to_string(),
            codex_home: self.home.path().display().to_string(),
            runtime: self.runtime.try_lock().ok().and_then(|g| g.clone()),
            cached_threads: self.thread_cache.len(),
            pending_count: self
                .pending_codex_threads_handle
                .as_ref()
                .map(|pending| pending.pending_count_snapshot())
                .unwrap_or(0),
            restart_count: self.restart_count.load(Ordering::SeqCst),
            last_error: self.last_error.try_lock().ok().and_then(|g| g.clone()),
        }
    }

    pub fn cached_card_for_thread(&self, thread_id: &str) -> Option<String> {
        self.thread_cache.get(thread_id).map(|v| v.value().clone())
    }

    #[cfg(feature = "fixtures")]
    pub fn active_turn_for_test(&self, thread_id: &str) -> Option<String> {
        self.active_turns
            .get(thread_id)
            .map(|entry| entry.value().clone())
    }

    #[cfg(feature = "fixtures")]
    pub fn set_active_turn_for_test(&self, thread_id: &str, turn_id: &str) {
        self.active_turns
            .insert(thread_id.to_string(), turn_id.to_string());
    }

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
        h.update(ingest_url.as_bytes());
        h.update(b"|");
        h.update(http_proxy.unwrap_or_default().as_bytes());
        h.update(b"|");
        h.update(https_proxy.unwrap_or_default().as_bytes());
        let hex = hex::encode(h.finalize());
        hex[..16].to_string()
    }

    async fn client(&self) -> Result<Arc<CodexAppServer>> {
        self.client
            .lock()
            .await
            .clone()
            .ok_or_else(|| CalmError::CodexAppServer("shared app-server is not connected".into()))
    }

    async fn current_env_signature(&self) -> Result<String> {
        let settings = load_settings(self.repo.as_ref()).await?;
        let http_proxy = Self::effective_proxy_env(
            settings.http_proxy.as_deref(),
            &["HTTP_PROXY", "http_proxy"],
        );
        let https_proxy = Self::effective_proxy_env(
            settings.https_proxy.as_deref(),
            &["HTTPS_PROXY", "https_proxy"],
        );
        Ok(Self::compute_env_signature(
            &self.ingest_url,
            http_proxy.as_deref(),
            https_proxy.as_deref(),
        ))
    }

    async fn try_takeover_live(&self, record: &crate::db::SharedCodexDaemonRecord) -> Result<bool> {
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
        let current_env_signature = self.current_env_signature().await?;
        if record.daemon_env_signature.as_deref() != Some(current_env_signature.as_str()) {
            tracing::warn!(
                target: "shared_codex_daemon::takeover_env_changed",
                pid,
                pgid,
                persisted = ?record.daemon_env_signature,
                current = %current_env_signature,
                "shared daemon was spawned with stale env signature; reaping for respawn"
            );
            reap_verified_process_group(pid, pgid, start_time, &boot_id).await;
            return Ok(false);
        }
        let Some(sock_path) = &record.sock_path else {
            return Ok(false);
        };
        let sock = PathBuf::from(sock_path);
        match connect_initialized(&sock).await {
            Ok((client, notifications)) => {
                let client = Arc::new(client);
                self.install_client(client, notifications).await;
                *self.runtime.lock().await = Some(SharedDaemonRuntime {
                    pid,
                    pgid,
                    boot_id,
                    process_start_time: start_time,
                    started_at,
                });
                *self.state.lock().await = SharedDaemonState::Running;
                self.resume_cached_threads().await;
                Ok(true)
            }
            Err(e) => {
                tracing::warn!(
                    target: "shared_codex_daemon::stop",
                    pid,
                    pgid,
                    error = %e,
                    "takeover handshake failed against verified daemon; reaping pgid before relaunch"
                );
                reap_verified_process_group(pid, pgid, start_time, &boot_id).await;
                Ok(false)
            }
        }
    }

    async fn start_new_process(
        &self,
        state: SharedDaemonState,
        increment_restart_count: bool,
        last_error: Option<String>,
    ) -> Result<()> {
        let _guard = self.restart_lock.lock().await;
        *self.state.lock().await = state;
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
        cmd.arg("app-server")
            .arg("--listen")
            .arg(&listen)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .process_group(0)
            .kill_on_drop(true);
        self.apply_spawn_env(&mut cmd).await?;
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
        let daemon_env_signature = self.current_env_signature().await?;
        let runtime = SharedDaemonRuntime {
            pid,
            pgid,
            boot_id: boot_id.clone(),
            process_start_time,
            started_at,
        };
        let mut spawn_guard = SpawnedChildGuard::new(child, pgid);

        self.persist_runtime_starting(&runtime, last_error.clone(), daemon_env_signature.clone())
            .await?;

        let (client, notifications) = self.poll_connect_initialized().await?;
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
                daemon_env_signature: Some(daemon_env_signature),
            })
            .await?;
        let child = spawn_guard.disarm();
        *self.child.lock().await = Some(child);
        let client = Arc::new(client);
        self.install_client(client, notifications).await;
        *self.runtime.lock().await = Some(runtime.clone());
        *self.state.lock().await = SharedDaemonState::Running;
        self.restart_backoff.note_relaunch_now();
        if increment_restart_count {
            self.restart_count.fetch_add(1, Ordering::SeqCst);
        }
        *self.last_error.lock().await = last_error.clone();
        self.resume_cached_threads().await;
        tracing::info!(
            target = "shared_codex_daemon::start",
            boot_id = %boot_id,
            pgid,
            sock = %self.sock.display(),
            home = %self.home.path().display(),
            "shared codex app-server running"
        );
        Ok(())
    }

    async fn apply_spawn_env(&self, cmd: &mut Command) -> Result<()> {
        for stale in [
            "NEIGE_CARD_ID",
            "NEIGE_HOOK_PROVIDER",
            "NEIGE_MCP_TOKEN",
            "NEIGE_HOOK_URL",
        ] {
            cmd.env_remove(stale);
        }

        cmd.env("CODEX_HOME", self.home.path())
            .env("NEIGE_CALM_BASE_URL", &self.ingest_url);

        let settings = load_settings(self.repo.as_ref()).await?;
        if let Some(p) = settings.http_proxy.as_deref().filter(|s| !s.is_empty()) {
            cmd.env("HTTP_PROXY", p).env("http_proxy", p);
        }
        if let Some(p) = settings.https_proxy.as_deref().filter(|s| !s.is_empty()) {
            cmd.env("HTTPS_PROXY", p).env("https_proxy", p);
        }

        Ok(())
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
        *self.client.lock().await = None;
        *self.state.lock().await = SharedDaemonState::Restarting;

        self.reap_current_child_or_runtime().await;

        self.start_new_process(SharedDaemonState::Restarting, true, None)
            .await?;
        self.spawn_spawned_child_watcher();
        Ok(())
    }

    async fn reap_current_child_or_runtime(&self) {
        let runtime = self.runtime.lock().await.take();
        let child = self.child.lock().await.take();
        if let Some(mut child) = child {
            let pgid = runtime
                .as_ref()
                .map(|runtime| runtime.pgid)
                .or_else(|| child.id().and_then(|pid| i32::try_from(pid).ok()));
            if let Some(pgid) = pgid {
                signal_process_group(pgid, libc::SIGTERM);
            }
            match tokio::time::timeout(Duration::from_millis(500), child.wait()).await {
                Ok(Ok(_status)) => return,
                Ok(Err(e)) => {
                    tracing::warn!(
                        target: "shared_codex_daemon::stop",
                        ?pgid,
                        error = %e,
                        "failed waiting for shared codex app-server after SIGTERM; escalating"
                    );
                }
                Err(_) => {}
            }
            if let Some(pgid) = pgid {
                signal_process_group(pgid, libc::SIGKILL);
            }
            let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
            return;
        }

        let Some(runtime) = runtime else {
            return;
        };
        self.abort_taken_over_pid_watcher().await;
        reap_verified_process_group(
            runtime.pid,
            runtime.pgid,
            runtime.process_start_time,
            &runtime.boot_id,
        )
        .await;
    }

    async fn remove_stale_socket_before_spawn(&self) -> Result<()> {
        if self.sock.exists() {
            reap_listener_if_alive(&self.sock).await?;
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
            .await
    }

    async fn poll_connect_initialized(
        &self,
    ) -> Result<(CodexAppServer, crate::codex_appserver::NotificationStream)> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match connect_initialized(&self.sock).await {
                Ok(pair) => return Ok(pair),
                Err(e) if tokio::time::Instant::now() < deadline => {
                    let _ = e;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn install_client(
        &self,
        client: Arc<CodexAppServer>,
        mut notifications: crate::codex_appserver::NotificationStream,
    ) {
        *self.client.lock().await = Some(client);
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

    async fn rebuild_thread_cache_from_db(&self) -> Result<()> {
        self.thread_cache.clear();
        self.active_turns.clear();
        for row in self.repo.card_codex_threads_active_shared_only().await? {
            self.thread_cache.insert(row.thread_id, row.card_id);
        }
        Ok(())
    }

    async fn resume_cached_threads(&self) {
        let Some(client) = self.client.lock().await.clone() else {
            return;
        };
        for entry in self.thread_cache.iter() {
            let thread_id = entry.key().clone();
            tracing::info!(
                target = "shared_codex_daemon::resume",
                %thread_id,
                "resuming shared codex thread"
            );
            if let Err(e) = client.thread_resume(&thread_id).await {
                tracing::warn!(
                    target = "shared_codex_daemon::resume",
                    %thread_id,
                    error = %e,
                    "shared codex thread resume failed; leaving mapping intact"
                );
            }
        }
    }

    fn spawn_spawned_child_watcher(self: &Arc<Self>) {
        if self.monitor_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = Arc::downgrade(self);
        tokio::spawn(async move {
            Self::watch_spawned_child(this).await;
        });
    }

    async fn spawn_taken_over_pid_watcher(self: &Arc<Self>, runtime: SharedDaemonRuntime) {
        if self.monitor_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            Self::watch_taken_over_pid(this.clone(), runtime).await;
            if let Some(this) = this.upgrade() {
                *this.taken_over_pid_watcher.lock().await = None;
                Self::watch_spawned_child(Arc::downgrade(&this)).await;
                return;
            }
            Self::watch_spawned_child(this).await;
        });
        *self.taken_over_pid_watcher.lock().await = Some(handle);
    }

    async fn abort_taken_over_pid_watcher(&self) {
        if let Some(handle) = self.taken_over_pid_watcher.lock().await.take() {
            handle.abort();
            self.monitor_started.store(false, Ordering::SeqCst);
        }
    }

    async fn watch_spawned_child(this: std::sync::Weak<Self>) {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let Some(this) = this.upgrade() else {
                return;
            };
            let exited = {
                let mut guard = this.child.lock().await;
                match guard
                    .as_mut()
                    .and_then(|child| child.try_wait().ok())
                    .flatten()
                {
                    Some(status) => {
                        *guard = None;
                        Some(status)
                    }
                    None => None,
                }
            };
            if let Some(status) = exited {
                let uptime_sec = this
                    .runtime
                    .lock()
                    .await
                    .as_ref()
                    .map(|runtime| (now_ms() - runtime.started_at).max(0) / 1000)
                    .unwrap_or(0);
                let error = format!("shared codex app-server exited: {status}");
                tracing::warn!(
                    target = "shared_codex_daemon::stop",
                    uptime_sec,
                    exit_code = status.code(),
                    signal = status.signal(),
                    "shared codex app-server stopped"
                );
                this.restart_after_crash(error).await;
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
                this.restart_after_crash(error).await;
                return;
            }
        }
    }

    async fn restart_after_crash(&self, error: String) {
        *self.client.lock().await = None;
        *self.runtime.lock().await = None;
        *self.state.lock().await = SharedDaemonState::Restarting;
        *self.last_error.lock().await = Some(error.clone());
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
        if let Err(e) = self
            .start_new_process(SharedDaemonState::Restarting, true, Some(error.clone()))
            .await
        {
            *self.state.lock().await = SharedDaemonState::Failed;
            *self.last_error.lock().await = Some(e.to_string());
            let _ = self
                .repo
                .shared_daemon_runtime_set(SharedCodexDaemonUpdate {
                    state: SharedDaemonState::Failed.as_db_str().to_string(),
                    pid: None,
                    pgid: None,
                    sock_path: Some(self.sock.display().to_string()),
                    codex_home_path: Some(self.home.path().display().to_string()),
                    process_start_time: None,
                    boot_id: None,
                    started_at: None,
                    last_error: Some(e.to_string()),
                    increment_restart_count: false,
                    daemon_env_signature: Some(self.current_env_signature().await.unwrap_or_else(
                        |_| Self::compute_env_signature(&self.ingest_url, None, None),
                    )),
                })
                .await;
        }
    }
}

impl SharedCodexAppServer {
    /// #480 §C — typestate transition: begin/finish a fresh process spawn.
    /// **Invariant**: must hold `transition_serial` for the duration.
    /// PR5a stub: parallel-writes the new state but does NOT replace the
    /// existing `start_new_process` impl. PR5b makes this the canonical
    /// path.
    #[allow(dead_code)]
    pub(crate) async fn start_new_process_typestate<F, Fut>(&self, spawn: F) -> anyhow::Result<()>
    where
        F: FnOnce(PathBuf) -> Fut + Send,
        Fut: std::future::Future<Output = anyhow::Result<LaunchedSharedDaemon>> + Send,
    {
        let _serial = self.transition_serial.lock().await;
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
                let mut core = self.core.lock().await;
                core.state = SupervisorState::Running {
                    child: launched.child,
                    client: launched.client,
                    runtime: launched.runtime,
                    watcher: launched.watcher,
                };
                Ok(())
            }
            Err(err) => {
                let mut core = self.core.lock().await;
                core.state = SupervisorState::Failed {
                    last_error: err.to_string(),
                    since: Instant::now(),
                };
                Err(err)
            }
        }
    }

    /// #480 §C — typestate transition: mark a restart in progress.
    /// **Invariant**: must hold `transition_serial`.
    #[allow(dead_code)]
    pub(crate) async fn begin_restart(&self, prev_pid: Option<i32>, reason: String) {
        let _serial = self.transition_serial.lock().await;
        let mut core = self.core.lock().await;
        core.attempts = core.attempts.saturating_add(1);
        let attempts = core.attempts;
        core.state = SupervisorState::Restarting {
            prev_pid,
            reason,
            attempts,
        };
    }
}

async fn reap_verified_process_group(pid: i32, pgid: i32, start_time: u64, boot_id: &str) {
    signal_process_group(pgid, libc::SIGTERM);
    tokio::time::sleep(Duration::from_millis(500)).await;
    signal_process_group(pgid, libc::SIGKILL);
    tokio::time::sleep(Duration::from_millis(200)).await;

    if verify_owned_pid(pid, start_time, boot_id) {
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

fn thread_id_from_started(params: &serde_json::Value) -> Option<&str> {
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

fn other_thread_id(params: &serde_json::Value) -> Option<&str> {
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
    } else if let Some(row) = repo.card_codex_thread_get_by_thread(thread_id).await? {
        thread_cache.insert(thread_id.to_string(), row.card_id);
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

async fn reap_listener_if_alive(sock_path: &Path) -> Result<()> {
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
        unsafe {
            libc::kill(peer_pid, libc::SIGTERM);
        }
        drop(stream);
        tokio::time::sleep(Duration::from_millis(500)).await;
        unsafe {
            libc::kill(peer_pid, libc::SIGKILL);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        return Ok(());
    }

    tracing::warn!(
        target: "shared_codex_daemon::stop",
        peer_pid,
        pgid,
        sock = %sock_path.display(),
        "stale socket has live listener; reaping orphaned daemon pgid before unlink"
    );
    signal_process_group(pgid, libc::SIGTERM);
    drop(stream);
    tokio::time::sleep(Duration::from_millis(500)).await;
    signal_process_group(pgid, libc::SIGKILL);
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(())
}

struct SpawnedChildGuard {
    child: Option<Child>,
    pgid: i32,
}

impl SpawnedChildGuard {
    fn new(child: Child, pgid: i32) -> Self {
        Self {
            child: Some(child),
            pgid,
        }
    }

    fn disarm(&mut self) -> Child {
        self.child.take().expect("spawn guard disarmed once")
    }
}

impl Drop for SpawnedChildGuard {
    fn drop(&mut self) {
        if self.child.is_none() {
            return;
        }
        tracing::warn!(
            target: "shared_codex_daemon::stop",
            pgid = self.pgid,
            "spawn aborted; reaping orphan pgid"
        );
        signal_process_group(self.pgid, libc::SIGTERM);
        signal_process_group(self.pgid, libc::SIGKILL);
    }
}

impl Drop for SharedCodexAppServer {
    fn drop(&mut self) {
        if let Ok(runtime) = self.runtime.try_lock()
            && let Some(runtime) = runtime.as_ref()
        {
            let _ = signal_process_group(runtime.pgid, libc::SIGTERM);
        }
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
impl SharedCodexAppServer {
    pub fn sock_path(&self) -> &Path {
        &self.sock
    }

    pub async fn spawn_env_for_test(
        &self,
    ) -> Result<std::collections::BTreeMap<String, Option<String>>> {
        let mut cmd = Command::new(&self.codex_bin);
        self.apply_spawn_env(&mut cmd).await?;
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

    pub async fn taken_over_pid_watcher_active_for_test(&self) -> bool {
        self.taken_over_pid_watcher.lock().await.is_some()
    }
}

#[cfg(any(test, feature = "fixtures"))]
pub fn drop_spawned_child_guard_for_test(child: Child, pgid: i32) {
    let _guard = SpawnedChildGuard::new(child, pgid);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
