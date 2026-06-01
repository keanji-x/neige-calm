//! Shared `codex app-server` supervisor (#410 PR4).
//!
//! PR4 only starts, supervises, and takes over a single daemon for the whole
//! server. It deliberately does not route any card traffic to this daemon yet;
//! later PRs switch callers over through the public methods here.

use std::os::unix::io::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::Serialize;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, broadcast};

use crate::codex_appserver::{ClientInfo, CodexAppServer, InputItem, Notification};
use crate::config::Config;
use crate::db::{Repo, SharedCodexDaemonUpdate};
use crate::error::{CalmError, Result};
use crate::model::{CardRole, now_ms};
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
    pub restart_count: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SharedThreadStartParams {
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
    restart_backoff: BackoffState,
    notifications: NotificationFanout,
    codex_bin: String,
    log_dir: PathBuf,
    enabled: bool,
    client: Arc<Mutex<Option<Arc<CodexAppServer>>>>,
    child: Arc<Mutex<Option<Child>>>,
    monitor_started: AtomicBool,
    restart_lock: Mutex<()>,
    restart_count: std::sync::atomic::AtomicU64,
    last_error: Arc<Mutex<Option<String>>>,
}

impl SharedCodexAppServer {
    pub fn new_stub(repo: Arc<dyn Repo>) -> Arc<Self> {
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
            restart_backoff: BackoffState::new(Duration::from_millis(250), Duration::from_secs(10)),
            notifications: tx,
            codex_bin: "codex".into(),
            log_dir: root.join("logs/shared-codex-appserver"),
            enabled: false,
            client: Arc::new(Mutex::new(None)),
            child: Arc::new(Mutex::new(None)),
            monitor_started: AtomicBool::new(false),
            restart_lock: Mutex::new(()),
            restart_count: std::sync::atomic::AtomicU64::new(0),
            last_error: Arc::new(Mutex::new(None)),
        })
    }

    pub fn new(cfg: &Config, home: Arc<SharedCodexHome>, repo: Arc<dyn Repo>) -> Arc<Self> {
        let data_dir = cfg.data_dir_resolved();
        let (tx, _) = broadcast::channel(1024);
        Arc::new(Self {
            state: Arc::new(Mutex::new(SharedDaemonState::Idle)),
            sock: data_dir.join("run/codex-appserver.sock"),
            home,
            runtime: Arc::new(Mutex::new(None)),
            repo,
            thread_cache: Arc::new(DashMap::new()),
            restart_backoff: BackoffState::new(
                Duration::from_millis(cfg.shared_codex_appserver_restart_initial_delay_ms),
                Duration::from_millis(cfg.shared_codex_appserver_restart_max_delay_ms),
            ),
            notifications: tx,
            codex_bin: cfg.codex_bin.clone(),
            log_dir: cfg.shared_codex_appserver_log_dir_resolved(),
            enabled: cfg.shared_codex_appserver_enabled,
            client: Arc::new(Mutex::new(None)),
            child: Arc::new(Mutex::new(None)),
            monitor_started: AtomicBool::new(false),
            restart_lock: Mutex::new(()),
            restart_count: std::sync::atomic::AtomicU64::new(0),
            last_error: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn start_or_takeover(self: &Arc<Self>) -> Result<()> {
        if !self.enabled {
            tracing::info!(
                target = "shared_codex_daemon::start",
                "shared codex app-server disabled; skipping boot"
            );
            return Ok(());
        }

        self.rebuild_thread_cache_from_db().await?;
        let record = self.repo.shared_daemon_runtime_get().await?;
        if self.try_takeover_live(&record).await? {
            if let Some(runtime) = self.runtime.lock().await.clone() {
                self.spawn_taken_over_pid_watcher(runtime);
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
        &self,
        card_id: &str,
        role: CardRole,
        wave_id: Option<&str>,
        params: SharedThreadStartParams,
    ) -> Result<String> {
        let client = self.client().await?;
        let thread = client
            .thread_start(params.developer_instructions.as_deref())
            .await?;
        let thread_id = thread
            .thread_id()
            .ok_or_else(|| CalmError::CodexAppServer("thread/start returned no thread.id".into()))?
            .to_string();
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
        turn.turn_id()
            .map(ToOwned::to_owned)
            .ok_or_else(|| CalmError::CodexAppServer("turn/start returned no turn.id".into()))
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
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
            restart_count: self.restart_count.load(Ordering::SeqCst),
            last_error: self.last_error.try_lock().ok().and_then(|g| g.clone()),
        }
    }

    pub fn cached_card_for_thread(&self, thread_id: &str) -> Option<String> {
        self.thread_cache.get(thread_id).map(|v| v.value().clone())
    }

    async fn client(&self) -> Result<Arc<CodexAppServer>> {
        self.client
            .lock()
            .await
            .clone()
            .ok_or_else(|| CalmError::CodexAppServer("shared app-server is not connected".into()))
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
            .env("CODEX_HOME", self.home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .process_group(0)
            .kill_on_drop(true);
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
        let mut spawn_guard = SpawnedChildGuard::new(child, pgid);

        self.persist_runtime_starting(
            pid,
            pgid,
            process_start_time,
            &boot_id,
            started_at,
            last_error.clone(),
        )
        .await?;

        let (client, notifications) = self.poll_connect_initialized().await?;
        let runtime = SharedDaemonRuntime {
            pid,
            pgid,
            boot_id: boot_id.clone(),
            process_start_time,
            started_at,
        };
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

    async fn remove_stale_socket_before_spawn(&self) -> Result<()> {
        if self.sock.exists() {
            reap_listener_if_alive(&self.sock).await?;
            let _ = std::fs::remove_file(&self.sock);
        }
        Ok(())
    }

    async fn persist_runtime_starting(
        &self,
        pid: i32,
        pgid: i32,
        process_start_time: u64,
        boot_id: &str,
        started_at: i64,
        last_error: Option<String>,
    ) -> Result<()> {
        self.repo
            .shared_daemon_runtime_set(SharedCodexDaemonUpdate {
                state: SharedDaemonState::Starting.as_db_str().to_string(),
                pid: Some(pid),
                pgid: Some(pgid),
                sock_path: Some(self.sock.display().to_string()),
                codex_home_path: Some(self.home.path().display().to_string()),
                process_start_time: Some(process_start_time),
                boot_id: Some(boot_id.to_string()),
                started_at: Some(started_at),
                last_error,
                increment_restart_count: false,
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
        tokio::spawn(async move {
            while let Some(notification) = notifications.recv().await {
                let _ = tx.send(notification);
            }
        });
    }

    async fn rebuild_thread_cache_from_db(&self) -> Result<()> {
        self.thread_cache.clear();
        for row in self.repo.card_codex_threads_active().await? {
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

    fn spawn_taken_over_pid_watcher(self: &Arc<Self>, runtime: SharedDaemonRuntime) {
        if self.monitor_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = Arc::downgrade(self);
        tokio::spawn(async move {
            Self::watch_taken_over_pid(this.clone(), runtime).await;
            Self::watch_spawned_child(this).await;
        });
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
                })
                .await;
        }
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
}

#[cfg(any(test, feature = "fixtures"))]
pub fn drop_spawned_child_guard_for_test(child: Child, pgid: i32) {
    let _guard = SpawnedChildGuard::new(child, pgid);
}
