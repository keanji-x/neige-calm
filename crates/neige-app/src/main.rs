use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::extract::Query;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};
use tokio::time::timeout;

mod apply;
mod config;
mod identity;
mod installed;
mod manifest;
mod package;
mod preflight;
mod source;
mod upgrade;

use config::{AppConfig, ServeOverrides, default_config_path, init_config};
use identity::SpawnIdentity;
use manifest::UnitName;
use manifest::{CurrentVersion, ReleaseManifest};
use package::{NamedPath, PackageConfig};
use preflight::PreflightMode;

#[derive(Parser, Debug)]
#[command(
    name = "neige-app",
    version,
    about = "neige-calm host application shell"
)]
struct Cli {
    #[command(subcommand)]
    command: CommandMode,
}

#[derive(Subcommand, Debug)]
enum CommandMode {
    /// Run or inspect the systemd-oriented service mode.
    #[command(subcommand)]
    System(SystemCommand),
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
enum SystemCommand {
    /// Run the supervisor and local admin API.
    Serve(SystemServeArgs),
    /// Print a systemd unit that runs `neige-app system serve`.
    #[command(alias = "print-unit")]
    Unit(SystemUnitArgs),
    /// Create a starter config.toml.
    InitConfig(SystemInitConfigArgs),
    /// Install config and user systemd unit files without starting services.
    Install(SystemInstallArgs),
    /// Verify and stage a local package without activating it.
    Upgrade(SystemUpgradeArgs),
    /// Roll current symlink back to previous.
    Rollback(SystemRollbackArgs),
    /// Check whether a local release manifest is compatible with a current install.
    Preflight(SystemPreflightArgs),
    /// Build a local directory package with manifest.json and file hashes.
    Package(SystemPackageArgs),
}

#[derive(Args, Debug, Clone)]
struct SystemServeArgs {
    /// Config file path. Defaults to ~/.config/neige-app/config.toml.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Local admin API listen address. Keep loopback-only unless a reverse
    /// proxy adds another authentication layer.
    #[arg(long, env = "NEIGE_APP_ADMIN_LISTEN")]
    admin_listen: Option<SocketAddr>,

    /// Bearer token for state-changing admin API calls. Can also be supplied
    /// through NEIGE_APP_ADMIN_TOKEN.
    #[arg(long, env = "NEIGE_APP_ADMIN_TOKEN")]
    admin_token: Option<String>,

    /// File containing the bearer token for state-changing admin API calls.
    #[arg(long, env = "NEIGE_APP_ADMIN_TOKEN_FILE")]
    admin_token_file: Option<PathBuf>,

    /// calm-server binary to supervise. Future release switching will point
    /// this at `<state-dir>/current/bin/calm-server`.
    #[arg(long, env = "NEIGE_APP_CHILD_BIN")]
    child_bin: Option<PathBuf>,

    /// calm-proc-supervisor binary to supervise beside calm-server.
    #[arg(long, env = "NEIGE_APP_PROC_SUPERVISOR_BIN")]
    proc_supervisor_bin: Option<PathBuf>,

    /// Address passed to calm-server as CALM_LISTEN.
    #[arg(long, env = "NEIGE_APP_CALM_LISTEN")]
    calm_listen: Option<String>,

    /// Optional web/dist path passed through as CALM_WEB_DIST.
    #[arg(long, env = "NEIGE_APP_CALM_WEB_DIST")]
    calm_web_dist: Option<PathBuf>,

    /// Optional SQLite URL passed through as CALM_DB_URL.
    #[arg(long, env = "NEIGE_APP_CALM_DB_URL")]
    calm_db_url: Option<String>,

    /// Optional data dir passed through as CALM_DATA_DIR.
    #[arg(long, env = "NEIGE_APP_CALM_DATA_DIR")]
    calm_data_dir: Option<PathBuf>,

    /// Optional MCP stdio shim path passed through as CALM_MCP_STDIO_SHIM_BIN.
    #[arg(long, env = "NEIGE_APP_CALM_MCP_STDIO_SHIM_BIN")]
    calm_mcp_stdio_shim_bin: Option<PathBuf>,

    /// Working directory for the supervised calm-server process.
    #[arg(long, env = "NEIGE_APP_CHILD_CWD")]
    child_cwd: Option<PathBuf>,

    /// Extra argv appended to the calm-server command.
    #[arg(long = "child-arg", env = "NEIGE_APP_CHILD_ARG")]
    child_args: Option<Vec<String>>,

    /// Delay before restarting calm-server after an unexpected exit.
    #[arg(long, env = "NEIGE_APP_RESTART_DELAY_MS")]
    restart_delay_ms: Option<u64>,

    /// SIGTERM grace window before SIGKILL on restart/shutdown.
    #[arg(long, env = "NEIGE_APP_STOP_GRACE_MS")]
    stop_grace_ms: Option<u64>,
}

#[derive(Args, Debug)]
struct SystemUnitArgs {
    /// Config file path used by ExecStart.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Absolute path to the installed neige-app binary.
    #[arg(long)]
    bin: Option<PathBuf>,

    /// Unit name used in Description.
    #[arg(long)]
    name: Option<String>,

    /// PATH baked into the generated systemd unit. Defaults to this process' PATH.
    #[arg(long)]
    path: Option<String>,
}

#[derive(Args, Debug)]
struct SystemInitConfigArgs {
    /// Config file path to create.
    #[arg(long)]
    config: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct SystemInstallArgs {
    /// Config file path to create/use.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Overwrite an existing user systemd unit file.
    #[arg(long)]
    force: bool,

    /// PATH baked into the generated systemd unit. Defaults to this process' PATH.
    #[arg(long)]
    path: Option<String>,
}

#[derive(Args, Debug)]
struct SystemUpgradeArgs {
    /// Config file path to load.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Local package directory containing manifest.json.
    #[arg(long)]
    package: Option<PathBuf>,

    /// Upgrade mode override. Defaults to auto-detect from manifest units.
    #[arg(long, value_enum)]
    mode: Option<PreflightMode>,

    /// Activate the staged release by switching current/previous symlinks.
    #[arg(long)]
    activate: bool,
}

#[derive(Args, Debug)]
struct SystemRollbackArgs {
    /// Config file path to load.
    #[arg(long)]
    config: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct SystemPreflightArgs {
    /// Upgrade mode to evaluate.
    #[arg(long, value_enum)]
    mode: PreflightMode,

    /// JSON captured from the current install. Usually `GET /api/version`.
    #[arg(long)]
    current_version: PathBuf,

    /// Target release manifest JSON.
    #[arg(long)]
    manifest: PathBuf,
}

#[derive(Args, Debug)]
struct SystemPackageArgs {
    /// Final package directory, or the package directory name when --out is set.
    #[arg(long)]
    release_dir: PathBuf,

    /// Optional parent directory. When set, package output is --out/basename(--release-dir).
    #[arg(long)]
    out: Option<PathBuf>,

    /// Stable release identifier written to manifest.json.
    #[arg(long, default_value = "local")]
    release_id: String,

    /// neige-app binary copied to bin/neige-app.
    #[arg(long)]
    app_bin: PathBuf,

    /// web/dist directory copied to web/dist.
    #[arg(long)]
    web_dist: PathBuf,

    /// Binary to copy into bin/, as NAME=PATH. Repeat for each bundle binary.
    #[arg(long = "bin", value_parser = package::parse_named_path)]
    bins: Vec<NamedPath>,
}

#[derive(Debug, Clone)]
struct SupervisorConfig {
    name: String,
    child_bin: PathBuf,
    child_cwd: Option<PathBuf>,
    child_args: Vec<String>,
    child_envs: Vec<(String, String)>,
    restart_delay: Duration,
    stop_grace: Duration,
    calm_listen: Option<String>,
    persist_identity_to: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    cfg: Arc<AppConfig>,
    supervisor: Arc<Supervisor>,
    proc_supervisor: Arc<Supervisor>,
    apply_lock: Arc<Mutex<()>>,
    admin_token: Option<Arc<str>>,
}

impl AppState {
    async fn status_snapshot(&self) -> StatusSnapshot {
        let calm_server = self.supervisor.process_status().await;
        let proc_supervisor = self.proc_supervisor.process_status().await;
        StatusSnapshot {
            desired_running: calm_server.desired_running,
            child_state: calm_server.child_state.clone(),
            child_pid: calm_server.child_pid,
            restart_count: calm_server.restart_count,
            last_exit: calm_server.last_exit.clone(),
            calm_listen: self.supervisor.cfg.calm_listen.clone().unwrap_or_default(),
            calm_server,
            proc_supervisor,
        }
    }
}

impl From<&AppConfig> for SupervisorConfig {
    fn from(cfg: &AppConfig) -> Self {
        calm_server_supervisor_config(cfg)
    }
}

fn calm_server_supervisor_config(cfg: &AppConfig) -> SupervisorConfig {
    let control_sock = cfg.proc_supervisor_sock();
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
        child_args: cfg.child.extra_args.clone(),
        child_envs,
        restart_delay: cfg.timing.restart_delay,
        stop_grace: cfg.timing.stop_grace,
        calm_listen: Some(cfg.child.calm_listen.clone()),
        persist_identity_to: None,
    }
}

fn proc_supervisor_config(cfg: &AppConfig) -> SupervisorConfig {
    SupervisorConfig {
        name: "calm-proc-supervisor".into(),
        child_bin: cfg.child.proc_supervisor_bin.clone(),
        child_cwd: cfg.child.cwd.clone(),
        child_args: vec![
            "--control-sock".into(),
            cfg.proc_supervisor_sock().display().to_string(),
        ],
        child_envs: Vec::new(),
        restart_delay: cfg.timing.restart_delay,
        stop_grace: cfg.timing.stop_grace,
        calm_listen: None,
        persist_identity_to: Some(cfg.calm_data_dir_resolved()),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusSnapshot {
    /// Deprecated compatibility field; mirrors `calmServer.desiredRunning`.
    desired_running: bool,
    /// Deprecated compatibility field; mirrors `calmServer.childState`.
    child_state: String,
    /// Deprecated compatibility field; mirrors `calmServer.childPid`.
    child_pid: Option<u32>,
    /// Deprecated compatibility field; mirrors `calmServer.restartCount`.
    restart_count: u64,
    /// Deprecated compatibility field; mirrors `calmServer.lastExit`.
    last_exit: Option<String>,
    calm_listen: String,
    calm_server: ProcessStatus,
    proc_supervisor: ProcessStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessStatus {
    desired_running: bool,
    child_state: String,
    child_pid: Option<u32>,
    restart_count: u64,
    last_exit: Option<String>,
    identity: Option<SpawnIdentity>,
}

#[derive(Debug)]
struct SupervisorState {
    desired_running: bool,
    shutdown_requested: bool,
    owns_child: bool,
    child_state: ChildState,
    child_pid: Option<u32>,
    restart_count: u64,
    last_exit: Option<String>,
    identity: Option<SpawnIdentity>,
}

#[derive(Debug, Clone, Copy)]
enum ChildState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Exited,
}

impl ChildState {
    fn as_str(self) -> &'static str {
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
struct Supervisor {
    cfg: SupervisorConfig,
    state: Mutex<SupervisorState>,
    changed: Notify,
}

impl Supervisor {
    fn new(cfg: SupervisorConfig) -> Arc<Self> {
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

    async fn run(self: Arc<Self>) {
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

    async fn spawn_child(&self) -> anyhow::Result<tokio::process::Child> {
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
            // System mode owns the whole calm-server process tree. Put the
            // child in a fresh process group so restart/shutdown signals reach
            // descendants spawned under the supervised kernel.
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

    async fn record_exit(&self, status: std::io::Result<ExitStatus>) {
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

    async fn process_status(&self) -> ProcessStatus {
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

    async fn restart(&self) -> anyhow::Result<ProcessStatus> {
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

    async fn stop_and_wait(&self) -> anyhow::Result<ProcessStatus> {
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

    async fn resume(&self) {
        let mut state = self.state.lock().await;
        state.desired_running = true;
        self.changed.notify_waiters();
    }

    async fn wait_for_spawn(&self, grace: Duration) -> anyhow::Result<ProcessStatus> {
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

    async fn adopt_identity(&self, identity: Option<SpawnIdentity>, child_pid: Option<u32>) {
        let mut state = self.state.lock().await;
        state.desired_running = false;
        state.child_state = ChildState::Running;
        state.child_pid = child_pid;
        state.owns_child = false;
        state.identity = identity;
        self.changed.notify_waiters();
    }

    async fn shutdown(&self) {
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

    async fn force_stop_and_wait(&self) -> anyhow::Result<ProcessStatus> {
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

    async fn terminate_current_child_wait_then_kill(&self, pid: u32) -> anyhow::Result<()> {
        terminate_child_tree_wait_then_kill(pid, self.cfg.stop_grace).await?;
        let _ = self.wait_pid_change(pid, Duration::from_secs(1)).await;
        Ok(())
    }

    async fn wait_pid_change(&self, pid: u32, grace: Duration) -> bool {
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,neige_app=debug")),
        )
        .init();

    match Cli::parse().command {
        CommandMode::System(SystemCommand::Serve(args)) => serve_system(args).await,
        CommandMode::System(SystemCommand::Unit(args)) => {
            let cfg = AppConfig::load(args.config.as_deref())?;
            let config_path = args
                .config
                .clone()
                .unwrap_or_else(|| cfg.config_path.clone());
            let bin = args.bin.unwrap_or_else(|| cfg.systemd.bin.clone());
            let name = args.name.unwrap_or_else(|| cfg.systemd.unit_name.clone());
            let path_env = resolve_systemd_path_env(args.path.as_deref())?;
            print!(
                "{}",
                render_systemd_unit(&name, &bin, &config_path, &path_env)?
            );
            Ok(())
        }
        CommandMode::System(SystemCommand::InitConfig(args)) => run_init_config(args),
        CommandMode::System(SystemCommand::Install(args)) => run_install(args),
        CommandMode::System(SystemCommand::Upgrade(args)) => run_upgrade(args),
        CommandMode::System(SystemCommand::Rollback(args)) => run_rollback(args),
        CommandMode::System(SystemCommand::Preflight(args)) => run_preflight_cli(args),
        CommandMode::System(SystemCommand::Package(args)) => run_package_cli(args),
    }
}

fn run_init_config(args: SystemInitConfigArgs) -> anyhow::Result<()> {
    let path = args.config.unwrap_or_else(default_config_path);
    init_config(&path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "created": path,
        }))?
    );
    Ok(())
}

fn run_install(args: SystemInstallArgs) -> anyhow::Result<()> {
    let config_path = args.config.unwrap_or_else(default_config_path);
    let config_created = if config_path.exists() {
        false
    } else {
        init_config(&config_path)?;
        true
    };
    let cfg = AppConfig::load(Some(&config_path))?;
    if let Some(parent) = cfg.systemd.unit_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    if cfg.systemd.unit_path.exists() && !args.force {
        anyhow::bail!(
            "systemd unit {} already exists; pass --force to overwrite",
            cfg.systemd.unit_path.display()
        );
    }
    let path_env = resolve_systemd_path_env(args.path.as_deref())?;
    warn_missing_spawn_tools(&path_env);
    let token_created = ensure_admin_token_file(cfg.admin.token_file.as_ref())?;
    let unit = render_systemd_unit(
        &cfg.systemd.unit_name,
        &cfg.systemd.bin,
        &config_path,
        &path_env,
    )?;
    std::fs::write(&cfg.systemd.unit_path, unit)
        .with_context(|| format!("write {}", cfg.systemd.unit_path.display()))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "config": config_path,
            "configCreated": config_created,
            "tokenCreated": token_created,
            "unit": cfg.systemd.unit_path,
            "nextSteps": [
                "systemctl --user daemon-reload",
                format!("systemctl --user enable --now {}", cfg.systemd.unit_name),
            ],
        }))?
    );
    Ok(())
}

fn run_upgrade(args: SystemUpgradeArgs) -> anyhow::Result<()> {
    let cfg = AppConfig::load(args.config.as_deref())?;
    let source_driven = args.package.is_none();
    let mode_override = if source_driven {
        source::source_mode(&cfg, args.mode)?
    } else {
        args.mode
    };
    let package_dir = match args.package {
        Some(path) => path,
        None => source::build_source_package(&cfg, args.mode)?,
    };
    let mode = match mode_override {
        Some(mode) => mode,
        None => upgrade::infer_package_mode(&package_dir)?,
    };
    if matches!(mode, PreflightMode::AppOnly) && args.activate {
        anyhow::bail!("app-only self-upgrade activation is not supported");
    }
    let stage = upgrade::stage_upgrade(&cfg, &package_dir, mode)?;
    let activation = if args.activate {
        Some(upgrade::activate_staged_release(
            &cfg,
            &stage.stage_dir,
            &stage.preflight,
            &stage.release_id,
        )?)
    } else {
        None
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "packageDir": package_dir,
            "stage": stage,
            "activation": activation,
            "nextSteps": upgrade_next_steps(&cfg, activation.as_ref()),
        }))?
    );
    Ok(())
}

fn upgrade_next_steps(
    cfg: &AppConfig,
    activation: Option<&upgrade::ActivationResult>,
) -> Vec<String> {
    match activation {
        Some(result) if result.restart_required => vec![
            format!(
                "curl -H \"Authorization: Bearer $(cat {})\" -X POST http://{}/restart",
                cfg.admin
                    .token_file
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<token-file>".into()),
                cfg.admin.listen
            ),
            format!("systemctl --user restart {}", cfg.systemd.unit_name),
        ],
        Some(_) => {
            vec!["No backend restart required; refresh open browser tabs or reload /calm/.".into()]
        }
        None => vec![
            "Review stage.preflight, then rerun with --activate to switch release symlinks.".into(),
        ],
    }
}

fn run_rollback(args: SystemRollbackArgs) -> anyhow::Result<()> {
    let cfg = AppConfig::load(args.config.as_deref())?;
    let result = upgrade::rollback_current(&cfg)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn run_preflight_cli(args: SystemPreflightArgs) -> anyhow::Result<()> {
    let result = match read_json::<CurrentVersion>(&args.current_version)
        .with_context(|| format!("read current version {}", args.current_version.display()))
    {
        Ok(current) => match read_json::<ReleaseManifest>(&args.manifest)
            .with_context(|| format!("read manifest {}", args.manifest.display()))
        {
            Ok(manifest) => preflight::run_preflight(args.mode, &current, &manifest),
            Err(err) => preflight::PreflightResult::deny(
                args.mode,
                format!("target manifest is missing, invalid, or incomplete: {err:#}"),
                "provide-valid-manifest",
            ),
        },
        Err(err) => preflight::PreflightResult::deny(
            args.mode,
            format!("current version JSON is missing, invalid, or incomplete: {err:#}"),
            "provide-valid-current-version",
        ),
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn run_package_cli(args: SystemPackageArgs) -> anyhow::Result<()> {
    let package_dir = package::build_package(&PackageConfig {
        release_dir: args.release_dir,
        out: args.out,
        release_id: args.release_id,
        app_bin: Some(args.app_bin),
        web_dist: Some(args.web_dist),
        bins: args.bins,
    })?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "packageDir": package_dir,
            "manifest": package_dir.join("manifest.json"),
        }))?
    );
    Ok(())
}

fn read_json<T>(path: &PathBuf) -> anyhow::Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn serve_system(args: SystemServeArgs) -> anyhow::Result<()> {
    let admin_token_override = args.admin_token.clone();
    let config_path = args.config.clone();
    let mut cfg = AppConfig::load(config_path.as_deref())?;
    cfg.apply_serve_overrides(ServeOverrides {
        admin_listen: args.admin_listen,
        admin_token_file: args.admin_token_file,
        child_bin: args.child_bin,
        proc_supervisor_bin: args.proc_supervisor_bin,
        calm_listen: args.calm_listen,
        calm_web_dist: args.calm_web_dist,
        calm_db_url: args.calm_db_url,
        calm_data_dir: args.calm_data_dir,
        calm_mcp_stdio_shim_bin: args.calm_mcp_stdio_shim_bin,
        child_cwd: args.child_cwd,
        child_args: args.child_args,
        restart_delay_ms: args.restart_delay_ms,
        stop_grace_ms: args.stop_grace_ms,
    });
    let admin_listen = cfg.admin.listen;
    let admin_token = load_admin_token(
        admin_token_override.as_deref(),
        cfg.admin.token_file.as_ref(),
    )?;
    let listener = tokio::net::TcpListener::bind(admin_listen)
        .await
        .with_context(|| format!("bind admin API on {admin_listen}"))?;

    let proc_control_sock = cfg.proc_supervisor_sock();
    let cfg = Arc::new(cfg);
    let proc_supervisor = Supervisor::new(proc_supervisor_config(&cfg));
    let supervisor = Supervisor::new(calm_server_supervisor_config(&cfg));
    let app_state = AppState {
        cfg: cfg.clone(),
        supervisor: supervisor.clone(),
        proc_supervisor: proc_supervisor.clone(),
        apply_lock: Arc::new(Mutex::new(())),
        admin_token,
    };
    let app = admin_router(app_state);

    tracing::info!(addr = %admin_listen, "neige-app system admin API listening");
    let proc_supervisor_task = if proc_supervisor_is_adoptable(&proc_control_sock).await {
        let child_pid = peer_pid_for_unix_socket(&proc_control_sock).await;
        let identity = read_supervisor_identity(&cfg.calm_data_dir_resolved());
        proc_supervisor.adopt_identity(identity, child_pid).await;
        None
    } else {
        let task = tokio::spawn(proc_supervisor.clone().run());
        wait_for_proc_supervisor(&proc_control_sock).await?;
        Some(task)
    };
    kill_orphan_calm_server_socket_holder(&cfg).await?;
    let supervisor_task = tokio::spawn(supervisor.clone().run());

    let server_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(supervisor.clone(), proc_supervisor.clone()))
        .await;

    supervisor.shutdown().await;
    proc_supervisor.shutdown().await;
    supervisor_task.await?;
    if let Some(task) = proc_supervisor_task {
        task.await?;
    }
    server_result?;
    Ok(())
}

fn admin_router(app_state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/restart", post(restart))
        .route("/upgrade/apply", post(upgrade_apply))
        .route("/upgrade/full-reboot", post(upgrade_full_reboot))
        .route("/upgrade/history", get(upgrade_history))
        .route("/upgrade/rollback", post(upgrade_rollback))
        .route("/upgrade/applied-id", get(upgrade_applied_id))
        .with_state(app_state)
}

async fn proc_supervisor_is_adoptable(sock: &Path) -> bool {
    sock.exists() && tokio::net::UnixStream::connect(sock).await.is_ok()
}

#[cfg(unix)]
async fn peer_pid_for_unix_socket(sock: &Path) -> Option<u32> {
    let stream = match tokio::net::UnixStream::connect(sock).await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(socket = %sock.display(), error = %err, "failed to connect for peer credentials");
            return None;
        }
    };
    match unix_stream_peer_pid(&stream) {
        Ok(pid) => Some(pid),
        Err(err) => {
            tracing::warn!(socket = %sock.display(), error = %err, "failed to read peer credentials");
            None
        }
    }
}

#[cfg(not(unix))]
async fn peer_pid_for_unix_socket(_sock: &Path) -> Option<u32> {
    None
}

async fn kill_orphan_calm_server_socket_holder(cfg: &AppConfig) -> anyhow::Result<()> {
    let sock = cfg.calm_mcp_kernel_sock();
    if !sock.exists() {
        return Ok(());
    }
    let Some(pid) = peer_pid_for_unix_socket(&sock).await else {
        return Ok(());
    };
    tracing::warn!(
        pid,
        socket = %sock.display(),
        "killing orphan calm-server socket holder before spawn"
    );
    kill_pid(pid)?;
    wait_for_unix_socket_peer_to_change(&sock, pid, cfg.timing.stop_grace).await;
    Ok(())
}

async fn wait_for_unix_socket_peer_to_change(sock: &Path, old_pid: u32, grace: Duration) {
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if peer_pid_for_unix_socket(sock).await != Some(old_pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn read_supervisor_identity(data_dir: &Path) -> Option<SpawnIdentity> {
    let path = identity::supervisor_identity_path(data_dir);
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(identity) => Some(identity),
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "failed to parse supervisor identity");
                None
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "failed to read supervisor identity");
            None
        }
    }
}

async fn wait_for_proc_supervisor(sock: &Path) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if tokio::net::UnixStream::connect(sock).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    anyhow::bail!("calm-proc-supervisor did not listen on {}", sock.display())
}

async fn shutdown_signal(supervisor: Arc<Supervisor>, proc_supervisor: Arc<Supervisor>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutdown requested");
    supervisor.shutdown().await;
    proc_supervisor.shutdown().await;
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "service": "neige-app" }))
}

async fn status(State(state): State<AppState>) -> Json<StatusSnapshot> {
    Json(state.status_snapshot().await)
}

async fn restart(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    require_bearer(&headers, state.admin_token.as_deref())?;
    state.supervisor.restart().await?;
    Ok((StatusCode::ACCEPTED, Json(state.status_snapshot().await)).into_response())
}

/// Applies are serialized with `try_lock` so a concurrent request receives an
/// immediate 409 instead of queueing behind a potentially minutes-long upgrade.
async fn upgrade_apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<apply::UpgradeRequest>,
) -> Result<Response, ApiError> {
    // Applies are serialized with `try_lock` so a second request gets an
    // immediate 409 instead of queueing behind a minutes-long upgrade.
    let _guard = state.apply_lock.try_lock().map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "apply_in_progress",
            "another upgrade is in progress",
        )
    })?;
    require_bearer(&headers, state.admin_token.as_deref())?;
    let response =
        apply::apply_upgrade(&state.cfg, &state.supervisor, &state.proc_supervisor, req).await?;
    let status = match response.result {
        apply::UpgradeResult::Committed
            if response.release_history_entry.executed_breaking_self_exec =>
        {
            let kill_proc_supervisor = response
                .release_history_entry
                .units_changed
                .contains(&UnitName::CalmProcSupervisor);
            schedule_exec_self(
                state.cfg.clone(),
                state.supervisor.clone(),
                state.proc_supervisor.clone(),
                kill_proc_supervisor,
            );
            StatusCode::ACCEPTED
        }
        apply::UpgradeResult::Committed | apply::UpgradeResult::DryRun => StatusCode::OK,
        apply::UpgradeResult::Rejected => StatusCode::BAD_REQUEST,
        apply::UpgradeResult::RolledBack => StatusCode::BAD_GATEWAY,
    };
    Ok((status, Json(response)).into_response())
}

/// Full reboot shares the upgrade serialization lock and rejects concurrent
/// apply/rollback work immediately instead of queueing.
async fn upgrade_full_reboot(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    // Full reboot is serialized with apply/rollback and rejects concurrent
    // upgrade work immediately rather than queueing behind it.
    let _guard = state.apply_lock.try_lock().map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "apply_in_progress",
            "another upgrade is in progress",
        )
    })?;
    require_bearer(&headers, state.admin_token.as_deref())?;
    let supervisor = state.supervisor.clone();
    let proc_supervisor = state.proc_supervisor.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Err(err) = supervisor.force_stop_and_wait().await {
            tracing::warn!(error = %err, "failed to stop calm-server before full reboot");
        }
        if let Err(err) = proc_supervisor.force_stop_and_wait().await {
            tracing::warn!(error = %err, "failed to stop proc-supervisor before full reboot");
        }
        std::process::exit(0);
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"status": "full-reboot-scheduled"})),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    limit: Option<usize>,
}

async fn upgrade_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, ApiError> {
    require_bearer(&headers, state.admin_token.as_deref())?;
    let limit = query.limit.unwrap_or(50).max(1);
    let entries = apply::read_release_history_blocking(&state.cfg, limit).await?;
    Ok((StatusCode::OK, Json(entries)).into_response())
}

#[derive(Debug, Deserialize)]
struct RollbackRequest {
    to: String,
}

/// Rollback mutates the same symlinks, installed state, history, and DB backup
/// paths as apply, so it uses the same non-queueing upgrade serialization lock.
async fn upgrade_rollback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RollbackRequest>,
) -> Result<Response, ApiError> {
    // Rollback mutates the same symlinks, installed state, history, and DB
    // backup paths as apply, so it also rejects concurrent upgrade work.
    let _guard = state.apply_lock.try_lock().map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "apply_in_progress",
            "another upgrade is in progress",
        )
    })?;
    require_bearer(&headers, state.admin_token.as_deref())?;
    let response = apply::rollback_last_preserving(&state.cfg, &state.supervisor, &req.to).await?;
    Ok((StatusCode::OK, Json(response)).into_response())
}

async fn upgrade_applied_id(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    // TODO(#396): this sentinel is currently admin-authenticated, which makes
    // unauthenticated browser polling awkward. Revisit with a proper frontend
    // refresh/event design instead of loosening auth in this PR.
    require_bearer(&headers, state.admin_token.as_deref())?;
    let release_id = apply::read_last_upgrade_id_blocking(&state.cfg).await?;
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "releaseId": release_id })),
    )
        .into_response())
}

fn schedule_exec_self(
    cfg: Arc<AppConfig>,
    supervisor: Arc<Supervisor>,
    proc_supervisor: Arc<Supervisor>,
    kill_proc_supervisor: bool,
) {
    tokio::spawn(async move {
        // Axum has no per-response flush hook here. This delay gives hyper time
        // to write the 202 body before this process image is replaced; a slow
        // or dropped client may still observe the connection close first and
        // should confirm success through /upgrade/history after reconnecting.
        tokio::time::sleep(Duration::from_millis(2000)).await;
        if let Err(err) = supervisor.force_stop_and_wait().await {
            tracing::warn!(error = %err, "failed to stop calm-server before exec-self");
        }
        if kill_proc_supervisor && let Err(err) = proc_supervisor.force_stop_and_wait().await {
            tracing::warn!(error = %err, "failed to stop proc-supervisor before exec-self");
        }
        exec_self(&cfg);
    });
}

#[cfg(unix)]
fn exec_self(cfg: &AppConfig) -> ! {
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.is_empty() {
        args.push("neige-app".into());
    }
    if args.len() == 1 {
        args.extend(["system".into(), "serve".into()]);
    }
    if !args
        .iter()
        .any(|arg| arg == std::ffi::OsStr::new("--config"))
    {
        args.extend(["--config".into(), cfg.config_path.clone().into_os_string()]);
    }
    let program = cfg.release.current_server.join("bin").join("neige-app");
    let err = std::process::Command::new(&program).args(&args[1..]).exec();
    tracing::error!(program = %program.display(), error = %err, "exec self failed");
    std::process::exit(127);
}

#[cfg(not(unix))]
fn exec_self(_cfg: &AppConfig) -> ! {
    tracing::error!("exec self is only implemented on Unix");
    std::process::exit(127);
}

fn load_admin_token(
    token: Option<&str>,
    token_file: Option<&PathBuf>,
) -> anyhow::Result<Option<Arc<str>>> {
    if let Some(token) = token {
        let token = token.trim();
        if !token.is_empty() {
            return Ok(Some(Arc::from(token)));
        }
    }
    if let Some(path) = token_file {
        let token = std::fs::read_to_string(path)
            .with_context(|| format!("read admin token file {}", path.display()))?;
        let token = token.trim();
        if !token.is_empty() {
            return Ok(Some(Arc::from(token)));
        }
    }
    Ok(None)
}

fn require_bearer(headers: &HeaderMap, configured_token: Option<&str>) -> Result<(), ApiError> {
    let Some(configured_token) = configured_token else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "admin_token_not_configured",
            "M1 refuses state-changing admin API calls unless NEIGE_APP_ADMIN_TOKEN or NEIGE_APP_ADMIN_TOKEN_FILE is configured.",
        ));
    };

    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "missing_bearer_token",
            "state-changing admin API calls require Authorization: Bearer <token>.",
        ));
    };
    let Ok(value) = value.to_str() else {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_bearer_token",
            "authorization header is not valid UTF-8.",
        ));
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_bearer_token",
            "state-changing admin API calls require Authorization: Bearer <token>.",
        ));
    };
    if token != configured_token {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_bearer_token",
            "bearer token did not match the configured admin token.",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            value.to_string(),
        )
    }
}

impl From<apply::ApplyError> for ApiError {
    fn from(value: apply::ApplyError) -> Self {
        Self::new(value.status, value.code, value.message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!(
            status = self.status.as_u16(),
            error = self.code,
            message = %self.message,
            "admin API request failed"
        );
        (
            self.status,
            Json(serde_json::json!({
                "error": self.code,
                "message": self.message,
            })),
        )
            .into_response()
    }
}

fn render_systemd_unit(
    name: &str,
    bin: &Path,
    config_path: &Path,
    path_env: &str,
) -> anyhow::Result<String> {
    validate_systemd_exec_path(bin, "systemd.bin")?;
    validate_systemd_exec_path(config_path, "config path")?;
    validate_systemd_path_env(path_env)?;
    let escaped_path_env = escape_systemd_environment_value(path_env);
    Ok(format!(
        "\
[Unit]
Description={name} user service
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
# M1 is system-only: the user systemd manager supervises neige-app; neige-app supervises calm-server.
# PATH must include user-local bin dirs (~/.local/bin etc.) so calm-server can spawn codex/claude.
Environment=\"PATH={escaped_path_env}\"
ExecStart={bin} system serve --config {config_path}
Restart=always
RestartSec=2

[Install]
WantedBy=default.target
",
        name = name,
        bin = bin.display(),
        config_path = config_path.display(),
        escaped_path_env = escaped_path_env,
    ))
}

fn resolve_systemd_path_env(override_path: Option<&str>) -> anyhow::Result<String> {
    let path_env = override_path
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
    validate_systemd_path_env(&path_env)?;
    Ok(path_env)
}

fn validate_systemd_path_env(path_env: &str) -> anyhow::Result<()> {
    if path_env.is_empty() {
        anyhow::bail!(
            "PATH for systemd unit must not be empty (set $PATH in the shell that runs `system install`, or pass --path explicitly)"
        );
    }
    if path_env.chars().any(|c| c.is_ascii_control()) {
        anyhow::bail!("PATH for systemd unit must not contain ASCII control characters");
    }
    if path_env.contains('%') {
        anyhow::bail!("PATH for systemd unit must not contain % systemd specifiers");
    }
    Ok(())
}

fn escape_systemd_environment_value(value: &str) -> String {
    // systemd.exec(5) Environment= takes a quoted, C-style-escaped value. "$" is literal in Environment= (no variable expansion), only specifier "%" expansion happens — and we reject "%" upstream.
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn warn_missing_spawn_tools(path_env: &str) {
    for tool in ["codex", "claude", "git"] {
        if !path_has_executable(path_env, tool) {
            eprintln!(
                "warning: {tool} not found on PATH ({path_env}); waves using {tool} will fail to start"
            );
        }
    }
}

fn path_has_executable(path_env: &str, tool: &str) -> bool {
    path_env
        .split(':')
        .map(|dir| Path::new(dir).join(tool))
        .any(|candidate| is_executable_file(&candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn validate_systemd_exec_path(path: &Path, label: &str) -> anyhow::Result<()> {
    let text = path.to_string_lossy();
    if text.is_empty()
        || text
            .chars()
            .any(|ch| ch == '%' || ch.is_whitespace() || ch.is_control())
    {
        anyhow::bail!("{label} must not contain %, whitespace, or control characters: {text}");
    }
    Ok(())
}

fn ensure_admin_token_file(path: Option<&PathBuf>) -> anyhow::Result<bool> {
    let Some(path) = path else {
        anyhow::bail!("admin.token_file must be configured before install");
    };
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut bytes = [0_u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .with_context(|| "read random bytes from /dev/urandom")?;
    let mut token = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut token, "{byte:02x}").expect("write token hex");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("create {}", path.display()))?;
        file.write_all(token.as_bytes())
            .with_context(|| format!("write {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("create {}", path.display()))?;
        file.write_all(token.as_bytes())
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(true)
}

fn format_exit(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "terminated by signal".to_string(),
    }
}

#[cfg(unix)]
async fn terminate_child_tree_wait_then_kill(pid: u32, grace: Duration) -> anyhow::Result<()> {
    terminate_child_tree(pid)?;
    if !wait_for_process_group_exit(pid, grace).await {
        kill_child_tree(pid)?;
        let _ = wait_for_process_group_exit(pid, Duration::from_secs(1)).await;
    }
    Ok(())
}

#[cfg(not(unix))]
async fn terminate_child_tree_wait_then_kill(_pid: u32, _grace: Duration) -> anyhow::Result<()> {
    anyhow::bail!("process signaling is not implemented on this platform")
}

#[cfg(unix)]
async fn wait_for_process_group_exit(pid: u32, grace: Duration) -> bool {
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
fn process_group_exists(pid: u32) -> bool {
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
fn terminate_child_tree(pid: u32) -> anyhow::Result<()> {
    signal_process_group(pid, libc::SIGTERM, "SIGTERM")
}

#[cfg(unix)]
fn kill_child_tree(pid: u32) -> anyhow::Result<()> {
    signal_process_group(pid, libc::SIGKILL, "SIGKILL")
}

#[cfg(unix)]
fn signal_process_group(pid: u32, signal: libc::c_int, name: &str) -> anyhow::Result<()> {
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
fn kill_pid(pid: u32) -> anyhow::Result<()> {
    signal_pid(pid, libc::SIGKILL, "SIGKILL")
}

#[cfg(unix)]
fn signal_pid(pid: u32, signal: libc::c_int, name: &str) -> anyhow::Result<()> {
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
fn unix_stream_peer_pid(stream: &tokio::net::UnixStream) -> anyhow::Result<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("getsockopt SO_PEERCRED");
    }
    cred.pid
        .try_into()
        .context("peer pid from SO_PEERCRED does not fit u32")
}

#[cfg(not(unix))]
fn terminate_child_tree(_pid: u32) -> anyhow::Result<()> {
    anyhow::bail!("process signaling is not implemented on this platform")
}

#[cfg(not(unix))]
fn kill_child_tree(_pid: u32) -> anyhow::Result<()> {
    anyhow::bail!("process signaling is not implemented on this platform")
}

#[cfg(not(unix))]
fn kill_pid(_pid: u32) -> anyhow::Result<()> {
    anyhow::bail!("process signaling is not implemented on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_unit_points_at_system_serve() {
        let unit = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige/bin/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/usr/local/bin:/usr/bin",
        )
        .expect("render unit");

        assert!(!unit.contains("User="));
        assert!(unit.contains("ExecStart=/opt/neige/bin/neige-app system serve"));
        assert!(unit.contains("--config /home/me/.config/neige-app/config.toml"));
        assert!(!unit.contains("--child-bin"));
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn systemd_unit_rejects_unsafe_exec_paths() {
        let err = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige app/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/usr/local/bin:/usr/bin",
        )
        .expect_err("unsafe path must fail");
        assert!(err.to_string().contains("whitespace"));
    }

    #[test]
    fn systemd_unit_rejects_percent_specifier_paths() {
        let err = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige%h/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/usr/local/bin:/usr/bin",
        )
        .expect_err("percent path must fail");
        assert!(err.to_string().contains("%"));
    }

    #[test]
    fn systemd_unit_includes_path_env() {
        let unit = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige/bin/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/foo/bin:/usr/bin",
        )
        .expect("render unit");

        assert_eq!(
            unit.matches("Environment=\"PATH=/foo/bin:/usr/bin\"")
                .count(),
            1
        );
        let env_pos = unit
            .find("Environment=\"PATH=/foo/bin:/usr/bin\"")
            .expect("PATH environment line");
        let exec_pos = unit.find("ExecStart=").expect("ExecStart line");
        assert!(env_pos < exec_pos);
    }

    #[test]
    fn systemd_unit_quotes_space_in_path() {
        let unit = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige/bin/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/opt/ai tools/bin:/usr/bin",
        )
        .expect("render unit");

        assert!(unit.contains("Environment=\"PATH=/opt/ai tools/bin:/usr/bin\"\n"));
    }

    #[test]
    fn systemd_unit_preserves_literal_dollar_in_path() {
        let unit = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige/bin/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/opt/x$y/bin",
        )
        .expect("render unit");

        assert!(
            unit.lines()
                .any(|line| line == "Environment=\"PATH=/opt/x$y/bin\"")
        );
    }

    #[test]
    fn systemd_unit_escapes_backslash_in_path() {
        let unit = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige/bin/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/a\\b/bin",
        )
        .expect("render unit");

        assert!(unit.contains("Environment=\"PATH=/a\\\\b/bin\"\n"));
    }

    #[test]
    fn systemd_unit_escapes_double_quote_in_path() {
        let unit = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige/bin/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/a\"b/bin",
        )
        .expect("render unit");

        assert!(unit.contains("Environment=\"PATH=/a\\\"b/bin\"\n"));
    }

    #[test]
    fn systemd_unit_rejects_empty_path() {
        let err = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige/bin/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "",
        )
        .expect_err("empty PATH must fail");
        assert!(err.to_string().contains("pass --path explicitly"));
    }

    #[test]
    fn systemd_unit_rejects_newline_in_path() {
        let err = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige/bin/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/foo/bin\n/usr/bin",
        )
        .expect_err("newline in PATH must fail");
        assert!(err.to_string().contains("control"));
    }

    #[test]
    fn systemd_unit_rejects_tab_in_path() {
        let err = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige/bin/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/foo/bin\t/usr/bin",
        )
        .expect_err("tab in PATH must fail");
        assert!(err.to_string().contains("control"));
    }

    #[test]
    fn systemd_unit_rejects_percent_in_path() {
        let err = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige/bin/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/foo/%h/bin:/usr/bin",
        )
        .expect_err("percent in PATH must fail");
        assert!(err.to_string().contains("%"));
    }

    #[test]
    fn systemd_unit_rejects_nul_in_path() {
        let err = render_systemd_unit(
            "neige-app",
            &PathBuf::from("/opt/neige/bin/neige-app"),
            &PathBuf::from("/home/me/.config/neige-app/config.toml"),
            "/foo/bin\0/usr/bin",
        )
        .expect_err("NUL in PATH must fail");
        assert!(err.to_string().contains("control"));
    }

    #[test]
    fn child_state_strings_are_stable_wire_values() {
        assert_eq!(ChildState::Stopped.as_str(), "stopped");
        assert_eq!(ChildState::Starting.as_str(), "starting");
        assert_eq!(ChildState::Running.as_str(), "running");
        assert_eq!(ChildState::Stopping.as_str(), "stopping");
        assert_eq!(ChildState::Exited.as_str(), "exited");
    }

    #[test]
    fn cli_shape_is_system_only() {
        assert!(Cli::try_parse_from(["neige-app", "system", "unit"]).is_ok());
        assert!(
            Cli::try_parse_from(["neige-app", "system", "print-unit", "--path", "/usr/bin"])
                .is_ok()
        );
        assert!(Cli::try_parse_from(["neige-app", "desktop", "serve"]).is_err());
        assert!(Cli::try_parse_from(["neige-app", "container", "serve"]).is_err());
    }

    #[tokio::test]
    async fn status_route_returns_supervisor_identity_shape() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let cfg = AppConfig::starter(PathBuf::from("/tmp/neige-app/config.toml"));
        let state = AppState {
            cfg: Arc::new(cfg),
            supervisor: Supervisor::new(SupervisorConfig {
                name: "calm-server".into(),
                child_bin: PathBuf::from("calm-server"),
                child_cwd: None,
                child_args: Vec::new(),
                child_envs: vec![("CALM_LISTEN".into(), "127.0.0.1:4040".into())],
                restart_delay: Duration::from_millis(1),
                stop_grace: Duration::from_millis(1),
                calm_listen: Some("127.0.0.1:4040".into()),
                persist_identity_to: None,
            }),
            proc_supervisor: Supervisor::new(SupervisorConfig {
                name: "calm-proc-supervisor".into(),
                child_bin: PathBuf::from("calm-proc-supervisor"),
                child_cwd: None,
                child_args: Vec::new(),
                child_envs: Vec::new(),
                restart_delay: Duration::from_millis(1),
                stop_grace: Duration::from_millis(1),
                calm_listen: None,
                persist_identity_to: None,
            }),
            apply_lock: Arc::new(Mutex::new(())),
            admin_token: Some(Arc::from("test-token")),
        };
        let app = admin_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/status")
                    .header(axum::http::header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("status response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert!(body["calmServer"].is_object());
        assert!(body["procSupervisor"].is_object());
        assert!(body["calmServer"]["identity"].is_null());
        assert!(body["procSupervisor"]["identity"].is_null());
    }

    #[tokio::test]
    async fn adopted_supervisor_status_keeps_peer_pid() {
        let supervisor = Supervisor::new(SupervisorConfig {
            name: "calm-proc-supervisor".into(),
            child_bin: PathBuf::from("calm-proc-supervisor"),
            child_cwd: None,
            child_args: Vec::new(),
            child_envs: Vec::new(),
            restart_delay: Duration::from_millis(1),
            stop_grace: Duration::from_millis(1),
            calm_listen: None,
            persist_identity_to: None,
        });

        supervisor.adopt_identity(None, Some(12345)).await;
        let status = supervisor.process_status().await;

        assert_eq!(status.child_state, "running");
        assert_eq!(status.child_pid, Some(12345));
        assert!(!status.desired_running);
    }

    #[test]
    fn bearer_gate_refuses_state_changes_when_token_is_not_configured() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer anything".parse().expect("valid auth header"),
        );

        let err = require_bearer(&headers, None).expect_err("missing configured token is fatal");
        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.code, "admin_token_not_configured");
    }

    #[test]
    fn bearer_gate_accepts_matching_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer expected".parse().expect("valid auth header"),
        );

        require_bearer(&headers, Some("expected")).expect("matching token");
    }

    #[test]
    fn install_refuses_existing_unit_without_force() {
        let tmp = test_temp_dir("install-existing-unit");
        let config_path = tmp.join("config.toml");
        let unit_path = tmp.join("neige-app.service");
        let token_path = tmp.join("admin.token");
        std::fs::write(
            &config_path,
            format!(
                r#"
[admin]
token_file = "{}"

[systemd]
unit_path = "{}"
bin = "/usr/local/bin/neige-app"
"#,
                token_path.display(),
                unit_path.display()
            ),
        )
        .expect("write config");
        std::fs::write(&unit_path, "existing").expect("write unit");

        let err = run_install(SystemInstallArgs {
            config: Some(config_path),
            force: false,
            path: Some("/usr/local/bin:/usr/bin".into()),
        })
        .expect_err("existing unit must fail");

        assert!(err.to_string().contains("already exists"));
        assert!(!token_path.exists());
    }

    #[test]
    fn install_creates_token_file() {
        let tmp = test_temp_dir("install-token");
        let config_path = tmp.join("config.toml");
        let unit_path = tmp.join("neige-app.service");
        let token_path = tmp.join("admin.token");
        std::fs::write(
            &config_path,
            format!(
                r#"
[admin]
token_file = "{}"

[systemd]
unit_path = "{}"
bin = "/usr/local/bin/neige-app"
"#,
                token_path.display(),
                unit_path.display()
            ),
        )
        .expect("write config");

        run_install(SystemInstallArgs {
            config: Some(config_path),
            force: false,
            path: Some("/usr/local/bin:/usr/bin".into()),
        })
        .expect("install");

        let token = std::fs::read_to_string(&token_path).expect("read token");
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(unit_path.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&token_path)
                .expect("token metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn web_only_activation_next_steps_do_not_restart_backend() {
        let cfg = AppConfig::starter(PathBuf::from("/tmp/neige-app/config.toml"));
        let activation = upgrade::ActivationResult {
            activated: true,
            mode: "web-only".into(),
            release_id: "web-1".into(),
            restart_required: false,
            changed_symlinks: Vec::new(),
            db_backup: None,
        };

        let steps = upgrade_next_steps(&cfg, Some(&activation));

        assert_eq!(steps.len(), 1);
        assert!(steps[0].contains("No backend restart required"));
        assert!(!steps.iter().any(|step| step.contains("/restart")));
        assert!(!steps.iter().any(|step| step.contains("systemctl")));
    }

    #[test]
    fn server_activation_next_steps_include_restart() {
        let cfg = AppConfig::starter(PathBuf::from("/tmp/neige-app/config.toml"));
        let activation = upgrade::ActivationResult {
            activated: true,
            mode: "server-only".into(),
            release_id: "server-1".into(),
            restart_required: true,
            changed_symlinks: Vec::new(),
            db_backup: None,
        };

        let steps = upgrade_next_steps(&cfg, Some(&activation));

        assert!(steps.iter().any(|step| step.contains("/restart")));
        assert!(steps.iter().any(|step| step.contains("systemctl")));
    }

    fn test_temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("neige-app-{name}-{}", std::process::id()));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("remove stale temp dir");
        }
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }
}
