//! The existing managed child owner and its launch/process-control primitives.
use crate::{
    config::AppConfig,
    identity::{self, SpawnIdentity},
};
use anyhow::Context;
use serde::Serialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};
use tokio::time::timeout;

#[derive(Debug, Clone)]
pub(crate) struct SupervisorConfig {
    pub(crate) name: String,
    pub(crate) child_bin: PathBuf,
    pub(crate) child_cwd: Option<PathBuf>,
    pub(crate) child_args: Vec<String>,
    pub(crate) child_envs: Vec<(String, String)>,
    pub(crate) restart_delay: Duration,
    pub(crate) stop_grace: Duration,
    pub(crate) calm_listen: Option<String>,
    pub(crate) persist_identity_to: Option<PathBuf>,
    /// Required, not defaulted: it is a term of the healthcheck boot deadline whose
    /// under-estimation rolls back healthy boots.
    pub(crate) boot_plugin_budget: Duration,
}

impl From<&AppConfig> for SupervisorConfig {
    fn from(cfg: &AppConfig) -> Self {
        calm_server_supervisor_config(cfg)
    }
}

pub(crate) fn calm_server_supervisor_config(cfg: &AppConfig) -> SupervisorConfig {
    let control_sock = cfg.proc_supervisor_sock();
    let mut child_args = cfg.child.extra_args.clone();
    child_args.extend([
        "--kernel-client-key-file".into(),
        crate::execution::key_path(cfg).display().to_string(),
    ]);
    if cfg.tailnet_unavailable {
        child_args.push("--private-tailnet-unavailable".into());
    } else if let Some(tailnet) = &cfg.tailnet {
        child_args.extend([
            "--private-tailnet-config".into(),
            tailnet.ingress_config().display().to_string(),
        ]);
    }
    for (flag, path) in [
        ("--plugins-dir", &cfg.child.plugins_dir),
        ("--plugins-data-dir", &cfg.child.plugins_data_dir),
    ] {
        if let Some(path) = path {
            child_args.extend([flag.to_owned(), path.display().to_string()]);
        }
    }
    let mut child_envs = vec![
        ("CALM_LISTEN".into(), cfg.child.calm_listen.clone()),
        (
            "CALM_PROC_SUPERVISOR_SOCK".into(),
            control_sock.display().to_string(),
        ),
        (
            "CALM_DEV_AUTOLOGIN".into(),
            if cfg.child.auth_dev_autologin {
                "true".into()
            } else {
                "false".into()
            },
        ),
    ];
    if let Some(web_dist) = &cfg.child.web_dist {
        child_envs.push(("CALM_WEB_DIST".into(), web_dist.display().to_string()));
    }
    if let Some(fe_dist) = &cfg.child.fe_dist {
        child_envs.push(("CALM_FE_DIST".into(), fe_dist.display().to_string()));
    }
    if let Some(db_url) = &cfg.child.db_url {
        child_envs.push(("CALM_DB_URL".into(), db_url.clone()));
    }
    if let Some(data_dir) = &cfg.child.data_dir {
        child_envs.push(("CALM_DATA_DIR".into(), data_dir.display().to_string()));
    }
    if let Some(shim) = &cfg.child.mcp_stdio_shim_bin {
        child_envs.push(("CALM_MCP_STDIO_SHIM_BIN".into(), shim.display().to_string()));
    }
    if let Some(username) = &cfg.child.auth_username {
        child_envs.push(("CALM_AUTH_USERNAME".into(), username.clone()));
    }
    if let Some(password) = &cfg.child.auth_password {
        child_envs.push(("CALM_AUTH_PASSWORD".into(), password.clone()));
    }
    SupervisorConfig {
        name: "calm-server".into(),
        child_bin: cfg.child.bin.clone(),
        child_cwd: cfg.child.cwd.clone(),
        child_args,
        child_envs,
        restart_delay: cfg.timing.restart_delay,
        stop_grace: cfg.timing.stop_grace,
        calm_listen: Some(cfg.child.calm_listen.clone()),
        persist_identity_to: None,
        boot_plugin_budget: cfg.timing.boot_plugin_budget,
    }
}

pub(crate) fn proc_supervisor_config(cfg: &AppConfig) -> SupervisorConfig {
    SupervisorConfig {
        name: "calm-proc-supervisor".into(),
        child_bin: cfg.child.proc_supervisor_bin.clone(),
        child_cwd: cfg.child.cwd.clone(),
        child_args: vec![
            "--control-sock".into(),
            cfg.proc_supervisor_sock().display().to_string(),
            "--execution-state-dir".into(),
            cfg.calm_data_dir_resolved()
                .join("execution")
                .display()
                .to_string(),
            "--kernel-client-key-file".into(),
            crate::execution::key_path(cfg).display().to_string(),
            "--stop-grace-ms".into(),
            cfg.timing.stop_grace.as_millis().to_string(),
        ],
        child_envs: Vec::new(),
        restart_delay: cfg.timing.restart_delay,
        stop_grace: cfg.timing.stop_grace,
        calm_listen: None,
        persist_identity_to: Some(cfg.calm_data_dir_resolved()),
        boot_plugin_budget: cfg.timing.boot_plugin_budget,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProcessStatus {
    pub(crate) desired_running: bool,
    pub(crate) child_state: String,
    pub(crate) child_pid: Option<u32>,
    pub(crate) restart_count: u64,
    pub(crate) last_exit: Option<String>,
    pub(crate) identity: Option<SpawnIdentity>,
}

#[derive(Debug)]
pub(crate) struct SupervisorState {
    pub(crate) desired_running: bool,
    pub(crate) shutdown_requested: bool,
    pub(crate) owns_child: bool,
    pub(crate) child_state: ChildState,
    pub(crate) child_pid: Option<u32>,
    pub(crate) restart_count: u64,
    pub(crate) last_exit: Option<String>,
    pub(crate) identity: Option<SpawnIdentity>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ChildState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Exited,
}

impl ChildState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ChildState::Stopped => "stopped",
            ChildState::Starting => "starting",
            ChildState::Running => "running",
            ChildState::Stopping => "stopping",
            ChildState::Exited => "exited",
        }
    }
}

const RESTART_ABORT_LIMIT: usize = 10;
const RESTART_ABORT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) struct Supervisor {
    pub(crate) cfg: SupervisorConfig,
    pub(crate) state: Mutex<SupervisorState>,
    pub(crate) changed: Notify,
}

impl Supervisor {
    pub(crate) fn new(cfg: SupervisorConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            state: Mutex::new(SupervisorState {
                desired_running: true,
                shutdown_requested: false,
                owns_child: false,
                child_state: ChildState::Stopped,
                child_pid: None,
                restart_count: 0,
                last_exit: None,
                identity: None,
            }),
            changed: Notify::new(),
        })
    }

    pub(crate) async fn run(self: Arc<Self>) {
        let mut restart_times = VecDeque::new();
        loop {
            {
                let state = self.state.lock().await;
                if state.shutdown_requested {
                    break;
                }
                if !state.desired_running {
                    drop(state);
                    self.changed.notified().await;
                    continue;
                }
            }

            match self.spawn_child().await {
                Ok(mut child) => {
                    let pid = child.id();
                    {
                        let mut state = self.state.lock().await;
                        state.child_pid = pid;
                        state.owns_child = true;
                        state.child_state = ChildState::Running;
                        state.last_exit = None;
                    }
                    self.changed.notify_waiters();

                    let status = child.wait().await;
                    self.record_exit(status).await;
                }
                Err(err) => {
                    {
                        let mut state = self.state.lock().await;
                        state.child_pid = None;
                        state.owns_child = false;
                        state.child_state = ChildState::Exited;
                        state.last_exit = Some(format!("spawn failed: {err:#}"));
                    }
                    tracing::error!(process = %self.cfg.name, error = %err, "failed to spawn child");
                }
            }

            let should_restart = {
                let state = self.state.lock().await;
                state.desired_running && !state.shutdown_requested
            };
            if should_restart {
                let now = std::time::Instant::now();
                restart_times.push_back(now);
                while restart_times
                    .front()
                    .is_some_and(|at| now.duration_since(*at) > RESTART_ABORT_WINDOW)
                {
                    restart_times.pop_front();
                }
                if restart_times.len() > RESTART_ABORT_LIMIT {
                    let mut state = self.state.lock().await;
                    state.desired_running = false;
                    tracing::error!(
                        process = %self.cfg.name,
                        restart_count = state.restart_count,
                        last_exit = ?state.last_exit,
                        "supervised child exceeded restart rate limit; stopping respawn"
                    );
                    self.changed.notify_waiters();
                    continue;
                }
                tokio::time::sleep(self.cfg.restart_delay).await;
                let mut state = self.state.lock().await;
                state.restart_count += 1;
            }
        }

        let mut state = self.state.lock().await;
        state.child_pid = None;
        state.owns_child = false;
        state.child_state = ChildState::Stopped;
        self.changed.notify_waiters();
    }

    pub(crate) async fn spawn_child(&self) -> anyhow::Result<tokio::process::Child> {
        {
            let mut state = self.state.lock().await;
            state.child_state = ChildState::Starting;
            state.child_pid = None;
            state.owns_child = false;
            state.identity = None;
        }
        self.changed.notify_waiters();

        let child_bin = self.cfg.child_bin.clone();
        let persist_identity_to = self.cfg.persist_identity_to.clone();
        let process_name = self.cfg.name.clone();
        let identity = tokio::task::spawn_blocking(move || match identity::capture(&child_bin) {
            Ok(identity) => {
                if let Some(data_dir) = &persist_identity_to
                    && let Err(err) = identity::write_supervisor_identity(data_dir, &identity)
                {
                    tracing::warn!(
                        process = %process_name,
                        error = %err,
                        "failed to persist supervisor identity"
                    );
                }
                Some(identity)
            }
            Err(err) => {
                tracing::warn!(
                    process = %process_name,
                    child_bin = %child_bin.display(),
                    error = %err,
                    "failed to capture spawn identity"
                );
                None
            }
        })
        .await
        .context("identity capture task panicked")?;
        {
            let mut state = self.state.lock().await;
            state.identity = identity;
        }
        self.changed.notify_waiters();

        let mut cmd = Command::new(&self.cfg.child_bin);
        cmd.args(&self.cfg.child_args)
            .envs(self.cfg.child_envs.clone());
        if let Some(cwd) = &self.cfg.child_cwd {
            cmd.current_dir(cwd);
        }
        #[cfg(unix)]
        {
            // Fresh process group so restart/shutdown signals reach descendants of the child.
            cmd.process_group(0);
        }

        tracing::info!(
            child_bin = %self.cfg.child_bin.display(),
            process = %self.cfg.name,
            "starting supervised child"
        );
        cmd.spawn()
            .with_context(|| format!("spawn {}", self.cfg.child_bin.display()))
    }

    pub(crate) async fn record_exit(&self, status: std::io::Result<ExitStatus>) {
        let msg = match status {
            Ok(status) => format_exit(status),
            Err(err) => format!("wait failed: {err}"),
        };
        tracing::warn!(process = %self.cfg.name, last_exit = %msg, "supervised child exited");
        let mut state = self.state.lock().await;
        state.child_pid = None;
        state.child_state = ChildState::Exited;
        state.last_exit = Some(msg);
        self.changed.notify_waiters();
    }

    pub(crate) async fn process_status(&self) -> ProcessStatus {
        let state = self.state.lock().await;
        ProcessStatus {
            desired_running: state.desired_running,
            child_state: state.child_state.as_str().to_string(),
            child_pid: state.child_pid,
            restart_count: state.restart_count,
            last_exit: state.last_exit.clone(),
            identity: state.identity.clone(),
        }
    }

    pub(crate) async fn restart(&self) -> anyhow::Result<ProcessStatus> {
        let pid = {
            let mut state = self.state.lock().await;
            state.desired_running = true;
            state.child_state = ChildState::Stopping;
            state.child_pid
        };
        self.changed.notify_waiters();
        if let Some(pid) = pid {
            self.terminate_current_child_wait_then_kill(pid).await?;
        }
        Ok(self.process_status().await)
    }

    pub(crate) async fn stop_and_wait(&self) -> anyhow::Result<ProcessStatus> {
        let pid = {
            let mut state = self.state.lock().await;
            state.desired_running = false;
            state.child_state = ChildState::Stopping;
            state.child_pid
        };
        self.changed.notify_waiters();
        if let Some(pid) = pid {
            self.terminate_current_child_wait_then_kill(pid).await?;
        }
        Ok(self.process_status().await)
    }

    pub(crate) async fn resume(&self) {
        let mut state = self.state.lock().await;
        state.desired_running = true;
        self.changed.notify_waiters();
    }

    pub(crate) async fn wait_for_spawn(&self, grace: Duration) -> anyhow::Result<ProcessStatus> {
        let wait = async {
            loop {
                let snapshot = self.process_status().await;
                if snapshot.child_state == "running" && snapshot.child_pid.is_some() {
                    return snapshot;
                }
                self.changed.notified().await;
            }
        };
        timeout(grace, wait)
            .await
            .with_context(|| format!("{} did not spawn within {:?}", self.cfg.name, grace))
    }

    pub(crate) async fn adopt_identity(
        &self,
        identity: Option<SpawnIdentity>,
        child_pid: Option<u32>,
    ) {
        let mut state = self.state.lock().await;
        state.desired_running = false;
        state.child_state = ChildState::Running;
        state.child_pid = child_pid;
        state.owns_child = false;
        state.identity = identity;
        self.changed.notify_waiters();
    }

    pub(crate) async fn shutdown(&self) {
        let pid = {
            let mut state = self.state.lock().await;
            state.desired_running = false;
            state.shutdown_requested = true;
            state.child_state = ChildState::Stopping;
            if state.owns_child {
                state.child_pid
            } else {
                None
            }
        };
        if let Some(pid) = pid
            && let Err(err) = self.terminate_current_child_wait_then_kill(pid).await
        {
            tracing::warn!(pid, error = %err, "failed to terminate child");
        }
    }

    pub(crate) async fn force_stop_and_wait(&self) -> anyhow::Result<ProcessStatus> {
        let pid = {
            let mut state = self.state.lock().await;
            state.desired_running = false;
            state.child_state = ChildState::Stopping;
            state.child_pid
        };
        self.changed.notify_waiters();
        if let Some(pid) = pid {
            self.terminate_current_child_wait_then_kill(pid).await?;
        }
        Ok(self.process_status().await)
    }

    pub(crate) async fn terminate_current_child_wait_then_kill(
        &self,
        pid: u32,
    ) -> anyhow::Result<()> {
        terminate_child_tree_wait_then_kill(pid, self.cfg.stop_grace).await?;
        let _ = self.wait_pid_change(pid, Duration::from_secs(1)).await;
        Ok(())
    }

    pub(crate) async fn wait_pid_change(&self, pid: u32, grace: Duration) -> bool {
        let wait = async {
            loop {
                {
                    let state = self.state.lock().await;
                    if state.child_pid != Some(pid) {
                        return;
                    }
                }
                self.changed.notified().await;
            }
        };
        timeout(grace, wait).await.is_ok()
    }
}

pub(crate) fn format_exit(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "terminated by signal".to_string(),
    }
}

#[cfg(unix)]
pub(crate) async fn terminate_child_tree_wait_then_kill(
    pid: u32,
    grace: Duration,
) -> anyhow::Result<()> {
    terminate_child_tree(pid)?;
    if !wait_for_process_group_exit(pid, grace).await {
        kill_child_tree(pid)?;
        let _ = wait_for_process_group_exit(pid, Duration::from_secs(1)).await;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) async fn terminate_child_tree_wait_then_kill(
    _pid: u32,
    _grace: Duration,
) -> anyhow::Result<()> {
    anyhow::bail!("process signaling is not implemented on this platform")
}

#[cfg(unix)]
pub(crate) async fn wait_for_process_group_exit(pid: u32, grace: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if !process_group_exists(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    !process_group_exists(pid)
}

#[cfg(unix)]
pub(crate) fn process_group_exists(pid: u32) -> bool {
    let Ok(raw_pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    let rc = unsafe { libc::kill(-raw_pid, 0) };
    if rc == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

#[cfg(unix)]
pub(crate) fn terminate_child_tree(pid: u32) -> anyhow::Result<()> {
    signal_process_group(pid, libc::SIGTERM, "SIGTERM")
}

#[cfg(unix)]
pub(crate) fn kill_child_tree(pid: u32) -> anyhow::Result<()> {
    signal_process_group(pid, libc::SIGKILL, "SIGKILL")
}

#[cfg(unix)]
pub(crate) fn signal_process_group(
    pid: u32,
    signal: libc::c_int,
    name: &str,
) -> anyhow::Result<()> {
    let raw_pid: libc::pid_t = pid
        .try_into()
        .with_context(|| format!("pid {pid} does not fit pid_t"))?;
    let process_group = -raw_pid;
    let rc = unsafe { libc::kill(process_group, signal) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
            .with_context(|| format!("send {name} to process group {pid}"))
    }
}

#[cfg(unix)]
pub(crate) fn kill_pid(pid: u32) -> anyhow::Result<()> {
    signal_pid(pid, libc::SIGKILL, "SIGKILL")
}

#[cfg(unix)]
pub(crate) fn signal_pid(pid: u32, signal: libc::c_int, name: &str) -> anyhow::Result<()> {
    let raw_pid: libc::pid_t = pid
        .try_into()
        .with_context(|| format!("pid {pid} does not fit pid_t"))?;
    let rc = unsafe { libc::kill(raw_pid, signal) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).with_context(|| format!("send {name} to pid {pid}"))
    }
}

#[cfg(unix)]
pub(crate) fn unix_stream_peer_pid(stream: &tokio::net::UnixStream) -> anyhow::Result<u32> {
    stream
        .peer_cred()
        .context("read Unix socket peer credentials")?
        .pid()
        .filter(|pid| *pid > 0)
        .context("Unix socket peer PID is unavailable")?
        .try_into()
        .context("Unix socket peer PID does not fit u32")
}

#[cfg(not(unix))]
pub(crate) fn terminate_child_tree(_pid: u32) -> anyhow::Result<()> {
    anyhow::bail!("process signaling is not implemented on this platform")
}

#[cfg(not(unix))]
pub(crate) fn kill_child_tree(_pid: u32) -> anyhow::Result<()> {
    anyhow::bail!("process signaling is not implemented on this platform")
}

#[cfg(not(unix))]
pub(crate) fn kill_pid(_pid: u32) -> anyhow::Result<()> {
    anyhow::bail!("process signaling is not implemented on this platform")
}
