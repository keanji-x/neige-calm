use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
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

mod config;
mod manifest;
mod package;
mod preflight;
mod source;
mod upgrade;

use config::{AppConfig, ServeOverrides, default_config_path, init_config};
use manifest::{Compatibility, CurrentVersion, DbMigrationPolicy, ReleaseManifest};
use package::{NamedPath, PackageConfig};
use preflight::PreflightMode;

#[derive(Parser, Debug)]
#[command(name = "neige-app", about = "neige-calm host application shell")]
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
enum SystemCommand {
    /// Run the supervisor and local admin API.
    Serve(SystemServeArgs),
    /// Print a systemd unit that runs `neige-app system serve`.
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

    /// Optional app shell version for units.app.
    #[arg(long)]
    app_version: Option<String>,

    /// Optional neige-app binary copied to bin/neige-app.
    #[arg(long)]
    app_bin: Option<PathBuf>,

    /// Optional web/dist directory copied to web/dist.
    #[arg(long)]
    web_dist: Option<PathBuf>,

    /// Optional web unit version.
    #[arg(long)]
    web_version: Option<String>,

    /// Optional calm-server unit version.
    #[arg(long)]
    calm_server_version: Option<String>,

    /// REST API compatibility version.
    #[arg(long)]
    api_version: String,

    /// Sync event compatibility version.
    #[arg(long)]
    sync_event_version: u32,

    /// MCP protocol compatibility version.
    #[arg(long)]
    mcp_protocol_version: String,

    /// Web bundle compatibility version.
    #[arg(long)]
    web_compat_version: u32,

    /// Minimum web compatibility accepted by the target server.
    #[arg(long)]
    min_web_compat_version: u32,

    /// DB migration policy: none, additive, forwardOnly, destructive.
    #[arg(long, default_value = "none", value_parser = parse_db_migration_policy)]
    db_migration_policy: DbMigrationPolicy,

    /// Binary to copy into bin/, as NAME=PATH. Repeat for each bundle binary.
    #[arg(long = "bin", value_parser = package::parse_named_path)]
    bins: Vec<NamedPath>,
}

#[derive(Debug, Clone)]
struct SupervisorConfig {
    child_bin: PathBuf,
    calm_listen: String,
    calm_web_dist: Option<PathBuf>,
    calm_db_url: Option<String>,
    calm_data_dir: Option<PathBuf>,
    calm_mcp_stdio_shim_bin: Option<PathBuf>,
    calm_auth_username: Option<String>,
    calm_auth_password: Option<String>,
    calm_auth_dev_autologin: bool,
    child_cwd: Option<PathBuf>,
    child_args: Vec<String>,
    restart_delay: Duration,
    stop_grace: Duration,
}

#[derive(Clone)]
struct AppState {
    supervisor: Arc<Supervisor>,
    admin_token: Option<Arc<str>>,
}

impl From<&AppConfig> for SupervisorConfig {
    fn from(cfg: &AppConfig) -> Self {
        Self {
            child_bin: cfg.child.bin.clone(),
            calm_listen: cfg.child.calm_listen.clone(),
            calm_web_dist: cfg.child.web_dist.clone(),
            calm_db_url: cfg.child.db_url.clone(),
            calm_data_dir: cfg.child.data_dir.clone(),
            calm_mcp_stdio_shim_bin: cfg.child.mcp_stdio_shim_bin.clone(),
            calm_auth_username: cfg.child.auth_username.clone(),
            calm_auth_password: cfg.child.auth_password.clone(),
            calm_auth_dev_autologin: cfg.child.auth_dev_autologin,
            child_cwd: cfg.child.cwd.clone(),
            child_args: cfg.child.extra_args.clone(),
            restart_delay: cfg.timing.restart_delay,
            stop_grace: cfg.timing.stop_grace,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusSnapshot {
    desired_running: bool,
    child_state: String,
    child_pid: Option<u32>,
    restart_count: u64,
    last_exit: Option<String>,
    calm_listen: String,
}

#[derive(Debug)]
struct SupervisorState {
    desired_running: bool,
    child_state: ChildState,
    child_pid: Option<u32>,
    restart_count: u64,
    last_exit: Option<String>,
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
                child_state: ChildState::Stopped,
                child_pid: None,
                restart_count: 0,
                last_exit: None,
            }),
            changed: Notify::new(),
        })
    }

    async fn run(self: Arc<Self>) {
        loop {
            {
                let state = self.state.lock().await;
                if !state.desired_running {
                    break;
                }
            }

            match self.spawn_child().await {
                Ok(mut child) => {
                    let pid = child.id();
                    {
                        let mut state = self.state.lock().await;
                        state.child_pid = pid;
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
                        state.child_state = ChildState::Exited;
                        state.last_exit = Some(format!("spawn failed: {err:#}"));
                    }
                    tracing::error!(error = %err, "failed to spawn calm-server");
                }
            }

            let should_restart = {
                let state = self.state.lock().await;
                state.desired_running
            };
            if should_restart {
                tokio::time::sleep(self.cfg.restart_delay).await;
                let mut state = self.state.lock().await;
                state.restart_count += 1;
            } else {
                break;
            }
        }

        let mut state = self.state.lock().await;
        state.child_pid = None;
        state.child_state = ChildState::Stopped;
        self.changed.notify_waiters();
    }

    async fn spawn_child(&self) -> anyhow::Result<tokio::process::Child> {
        {
            let mut state = self.state.lock().await;
            state.child_state = ChildState::Starting;
            state.child_pid = None;
        }
        self.changed.notify_waiters();

        let mut cmd = Command::new(&self.cfg.child_bin);
        cmd.args(&self.cfg.child_args)
            .env("CALM_LISTEN", &self.cfg.calm_listen);

        if let Some(web_dist) = &self.cfg.calm_web_dist {
            cmd.env("CALM_WEB_DIST", web_dist);
        }
        if let Some(db_url) = &self.cfg.calm_db_url {
            cmd.env("CALM_DB_URL", db_url);
        }
        if let Some(data_dir) = &self.cfg.calm_data_dir {
            cmd.env("CALM_DATA_DIR", data_dir);
        }
        if let Some(shim) = &self.cfg.calm_mcp_stdio_shim_bin {
            cmd.env("CALM_MCP_STDIO_SHIM_BIN", shim);
        }
        if let Some(username) = &self.cfg.calm_auth_username {
            cmd.env("CALM_AUTH_USERNAME", username);
        }
        if let Some(password) = &self.cfg.calm_auth_password {
            cmd.env("CALM_AUTH_PASSWORD", password);
        }
        cmd.env(
            "CALM_DEV_AUTOLOGIN",
            if self.cfg.calm_auth_dev_autologin {
                "true"
            } else {
                "false"
            },
        );
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
            calm_listen = %self.cfg.calm_listen,
            "starting calm-server child"
        );
        cmd.spawn()
            .with_context(|| format!("spawn {}", self.cfg.child_bin.display()))
    }

    async fn record_exit(&self, status: std::io::Result<ExitStatus>) {
        let msg = match status {
            Ok(status) => format_exit(status),
            Err(err) => format!("wait failed: {err}"),
        };
        tracing::warn!(last_exit = %msg, "calm-server child exited");
        let mut state = self.state.lock().await;
        state.child_pid = None;
        state.child_state = ChildState::Exited;
        state.last_exit = Some(msg);
        self.changed.notify_waiters();
    }

    async fn status(&self) -> StatusSnapshot {
        let state = self.state.lock().await;
        StatusSnapshot {
            desired_running: state.desired_running,
            child_state: state.child_state.as_str().to_string(),
            child_pid: state.child_pid,
            restart_count: state.restart_count,
            last_exit: state.last_exit.clone(),
            calm_listen: self.cfg.calm_listen.clone(),
        }
    }

    async fn restart(&self) -> anyhow::Result<StatusSnapshot> {
        let pid = {
            let mut state = self.state.lock().await;
            state.child_state = ChildState::Stopping;
            state.child_pid
        };
        if let Some(pid) = pid {
            terminate_child_tree(pid)?;
            let stopped = self.wait_pid_change(pid, self.cfg.stop_grace).await;
            if !stopped {
                kill_child_tree(pid)?;
            }
        }
        Ok(self.status().await)
    }

    async fn shutdown(&self) {
        let pid = {
            let mut state = self.state.lock().await;
            state.desired_running = false;
            state.child_state = ChildState::Stopping;
            state.child_pid
        };
        if let Some(pid) = pid {
            if let Err(err) = terminate_child_tree(pid) {
                tracing::warn!(pid, error = %err, "failed to SIGTERM child");
            }
            if !self.wait_pid_change(pid, self.cfg.stop_grace).await
                && let Err(err) = kill_child_tree(pid)
            {
                tracing::warn!(pid, error = %err, "failed to SIGKILL child");
            }
        }
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
            print!("{}", render_systemd_unit(&name, &bin, &config_path)?);
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
    let token_created = ensure_admin_token_file(cfg.admin.token_file.as_ref())?;
    let unit = render_systemd_unit(&cfg.systemd.unit_name, &cfg.systemd.bin, &config_path)?;
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
    let package_dir = match args.package {
        Some(path) => path,
        None => source::build_source_package(&cfg)?,
    };
    let mode = match args.mode {
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
        app_version: args.app_version,
        app_bin: args.app_bin,
        web_dist: args.web_dist,
        web_version: args.web_version,
        calm_server_version: args.calm_server_version,
        db_migration_policy: args.db_migration_policy,
        compatibility: Compatibility {
            api_version: args.api_version,
            sync_event_version: args.sync_event_version,
            mcp_protocol_version: args.mcp_protocol_version,
            web_compat_version: args.web_compat_version,
            min_web_compat_version: args.min_web_compat_version,
        },
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

fn parse_db_migration_policy(value: &str) -> Result<DbMigrationPolicy, String> {
    match value {
        "none" => Ok(DbMigrationPolicy::None),
        "additive" => Ok(DbMigrationPolicy::Additive),
        "forwardOnly" => Ok(DbMigrationPolicy::ForwardOnly),
        "destructive" => Ok(DbMigrationPolicy::Destructive),
        _ => Err("expected one of: none, additive, forwardOnly, destructive".into()),
    }
}

async fn serve_system(args: SystemServeArgs) -> anyhow::Result<()> {
    let admin_token_override = args.admin_token.clone();
    let config_path = args.config.clone();
    let mut cfg = AppConfig::load(config_path.as_deref())?;
    cfg.apply_serve_overrides(ServeOverrides {
        admin_listen: args.admin_listen,
        admin_token_file: args.admin_token_file,
        child_bin: args.child_bin,
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

    let supervisor = Supervisor::new(SupervisorConfig::from(&cfg));
    let app_state = AppState {
        supervisor: supervisor.clone(),
        admin_token,
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/restart", post(restart))
        .route("/update/apply", post(update_apply_placeholder))
        .with_state(app_state);

    tracing::info!(addr = %admin_listen, "neige-app system admin API listening");
    let supervisor_task = tokio::spawn(supervisor.clone().run());

    let server_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(supervisor.clone()))
        .await;

    supervisor.shutdown().await;
    supervisor_task.await?;
    server_result?;
    Ok(())
}

async fn shutdown_signal(supervisor: Arc<Supervisor>) {
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
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "service": "neige-app" }))
}

async fn status(State(state): State<AppState>) -> Json<StatusSnapshot> {
    Json(state.supervisor.status().await)
}

async fn restart(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    require_bearer(&headers, state.admin_token.as_deref())?;
    let status = state.supervisor.restart().await?;
    Ok((StatusCode::ACCEPTED, Json(status)).into_response())
}

async fn update_apply_placeholder(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(err) = require_bearer(&headers, state.admin_token.as_deref()) {
        return err.into_response();
    }
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "update_apply_not_implemented",
            "message": "M1 supports system service supervision; release apply will use the documented stage/verify/backup/activate/healthcheck/rollback state machine.",
        })),
    )
        .into_response()
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

fn render_systemd_unit(name: &str, bin: &Path, config_path: &Path) -> anyhow::Result<String> {
    validate_systemd_exec_path(bin, "systemd.bin")?;
    validate_systemd_exec_path(config_path, "config path")?;
    Ok(format!(
        "\
[Unit]
Description={name} user service
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
# M1 is system-only: the user systemd manager supervises neige-app; neige-app supervises calm-server.
ExecStart={bin} system serve --config {config_path}
Restart=always
RestartSec=2

[Install]
WantedBy=default.target
",
        name = name,
        bin = bin.display(),
        config_path = config_path.display(),
    ))
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

#[cfg(not(unix))]
fn terminate_child_tree(_pid: u32) -> anyhow::Result<()> {
    anyhow::bail!("process signaling is not implemented on this platform")
}

#[cfg(not(unix))]
fn kill_child_tree(_pid: u32) -> anyhow::Result<()> {
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
        )
        .expect_err("percent path must fail");
        assert!(err.to_string().contains("%"));
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
        assert!(Cli::try_parse_from(["neige-app", "desktop", "serve"]).is_err());
        assert!(Cli::try_parse_from(["neige-app", "container", "serve"]).is_err());
    }

    #[tokio::test]
    async fn update_apply_placeholder_is_explicitly_not_implemented_for_m1() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer test-token".parse().expect("valid auth header"),
        );
        let response =
            update_apply_placeholder(State(test_app_state(Some("test-token"))), headers).await;
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        let body = String::from_utf8(body.to_vec()).expect("utf8 response body");

        assert!(body.contains("update_apply_not_implemented"));
        assert!(body.contains("M1 supports system service supervision"));
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

    fn test_app_state(token: Option<&str>) -> AppState {
        AppState {
            supervisor: Supervisor::new(SupervisorConfig {
                child_bin: PathBuf::from("calm-server"),
                calm_listen: "127.0.0.1:4040".into(),
                calm_web_dist: None,
                calm_db_url: None,
                calm_data_dir: None,
                calm_mcp_stdio_shim_bin: None,
                calm_auth_username: None,
                calm_auth_password: None,
                calm_auth_dev_autologin: false,
                child_cwd: None,
                child_args: Vec::new(),
                restart_delay: Duration::from_millis(1),
                stop_grace: Duration::from_millis(1),
            }),
            admin_token: token.map(Arc::from),
        }
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
