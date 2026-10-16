//! Existing CLI composition, shared by the thin executable and the package owner.
use crate::{
    admin::serve_system,
    config::{AppConfig, SystemdScope, default_config_path, init_config},
    manifest::{CurrentVersion, ReleaseManifest},
    package::{self, NamedPath, PackageConfig},
    preflight::{self, PreflightMode},
    service_owner::*,
    setup, source, upgrade,
};
use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "neige-app",
    version,
    about = "neige-calm host application shell"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: CommandMode,
}

#[derive(Subcommand, Debug)]
pub(crate) enum CommandMode {
    /// Set up, run, or inspect the local application.
    #[command(subcommand)]
    System(SystemCommand),
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum SystemCommand {
    /// Install a verified package for first use, without requiring systemd.
    Setup(setup::SetupArgs),
    /// Run the supervisor and local admin API.
    Serve(SystemServeArgs),
    /// Print a systemd unit that runs `neige-app system serve`.
    #[command(alias = "print-unit")]
    Unit(SystemUnitArgs),
    /// Create a starter config.toml.
    InitConfig(SystemInitConfigArgs),
    /// Install config and systemd unit files without starting services.
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
pub(crate) struct SystemServeArgs {
    /// Config file path. Defaults to ~/.config/neige-app/config.toml.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,

    /// Local admin API listen address. Keep loopback-only unless a reverse
    /// proxy adds another authentication layer.
    #[arg(long, env = "NEIGE_APP_ADMIN_LISTEN")]
    pub(crate) admin_listen: Option<SocketAddr>,

    /// Bearer token for state-changing admin API calls. Can also be supplied
    /// through NEIGE_APP_ADMIN_TOKEN.
    #[arg(long, env = "NEIGE_APP_ADMIN_TOKEN")]
    pub(crate) admin_token: Option<String>,

    /// File containing the bearer token for state-changing admin API calls.
    #[arg(long, env = "NEIGE_APP_ADMIN_TOKEN_FILE")]
    pub(crate) admin_token_file: Option<PathBuf>,

    /// calm-server binary to supervise. Future release switching will point
    /// this at `<state-dir>/current/bin/calm-server`.
    #[arg(long, env = "NEIGE_APP_CHILD_BIN")]
    pub(crate) child_bin: Option<PathBuf>,

    /// calm-proc-supervisor binary to supervise beside calm-server.
    #[arg(long, env = "NEIGE_APP_PROC_SUPERVISOR_BIN")]
    pub(crate) proc_supervisor_bin: Option<PathBuf>,

    /// Address passed to calm-server as CALM_LISTEN.
    #[arg(long, env = "NEIGE_APP_CALM_LISTEN")]
    pub(crate) calm_listen: Option<String>,

    /// Optional web/dist path passed through as CALM_WEB_DIST.
    #[arg(long, env = "NEIGE_APP_CALM_WEB_DIST")]
    pub(crate) calm_web_dist: Option<PathBuf>,

    /// Next frontend directory passed to calm-server as CALM_FE_DIST.
    #[arg(long)]
    pub(crate) calm_fe_dist: Option<PathBuf>,

    /// Optional SQLite URL passed through as CALM_DB_URL.
    #[arg(long, env = "NEIGE_APP_CALM_DB_URL")]
    pub(crate) calm_db_url: Option<String>,

    /// Optional data dir passed through as CALM_DATA_DIR.
    #[arg(long, env = "NEIGE_APP_CALM_DATA_DIR")]
    pub(crate) calm_data_dir: Option<PathBuf>,

    /// Optional MCP stdio shim path passed through as CALM_MCP_STDIO_SHIM_BIN.
    #[arg(long, env = "NEIGE_APP_CALM_MCP_STDIO_SHIM_BIN")]
    pub(crate) calm_mcp_stdio_shim_bin: Option<PathBuf>,

    /// Working directory for the supervised calm-server process.
    #[arg(long, env = "NEIGE_APP_CHILD_CWD")]
    pub(crate) child_cwd: Option<PathBuf>,

    /// Extra argv appended to the calm-server command.
    #[arg(long = "child-arg", env = "NEIGE_APP_CHILD_ARG")]
    pub(crate) child_args: Option<Vec<String>>,

    /// Delay before restarting calm-server after an unexpected exit.
    #[arg(long, env = "NEIGE_APP_RESTART_DELAY_MS")]
    pub(crate) restart_delay_ms: Option<u64>,

    /// SIGTERM grace window before SIGKILL on restart/shutdown.
    #[arg(long, env = "NEIGE_APP_STOP_GRACE_MS")]
    pub(crate) stop_grace_ms: Option<u64>,
}

#[derive(Args, Debug)]
pub(crate) struct SystemUnitArgs {
    /// Config file path used by ExecStart.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,

    /// Absolute path to the installed neige-app binary.
    #[arg(long)]
    pub(crate) bin: Option<PathBuf>,

    /// Unit name used in Description.
    #[arg(long)]
    pub(crate) name: Option<String>,

    /// PATH baked into the generated systemd unit. Defaults to this process' PATH.
    #[arg(long)]
    pub(crate) path: Option<String>,
}

#[derive(Args, Debug)]
pub(crate) struct SystemInitConfigArgs {
    /// Config file path to create.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct SystemInstallArgs {
    /// Config file path to create/use.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,

    /// Overwrite an existing systemd unit file.
    #[arg(long)]
    pub(crate) force: bool,

    /// PATH baked into the generated systemd unit. Defaults to this process' PATH.
    #[arg(long)]
    pub(crate) path: Option<String>,

    /// User for system-scope units. Defaults to systemd.user, then SUDO_USER, then USER.
    #[arg(long)]
    pub(crate) user: Option<String>,
}

#[derive(Args, Debug)]
pub(crate) struct SystemUpgradeArgs {
    /// Config file path to load.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,

    /// Local package directory containing manifest.json.
    #[arg(long)]
    pub(crate) package: Option<PathBuf>,

    /// Upgrade mode override. Defaults to auto-detect from manifest units.
    #[arg(long, value_enum)]
    pub(crate) mode: Option<PreflightMode>,

    /// Activate the staged release by switching current/previous symlinks.
    #[arg(long)]
    pub(crate) activate: bool,
}

#[derive(Args, Debug)]
pub(crate) struct SystemRollbackArgs {
    /// Config file path to load.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct SystemPreflightArgs {
    /// Upgrade mode to evaluate.
    #[arg(long, value_enum)]
    pub(crate) mode: PreflightMode,

    /// JSON captured from the current install. Usually `GET /api/version`.
    #[arg(long)]
    pub(crate) current_version: PathBuf,

    /// Target release manifest JSON.
    #[arg(long)]
    pub(crate) manifest: PathBuf,
}

#[derive(Args, Debug)]
pub(crate) struct SystemPackageArgs {
    /// Final package directory, or the package directory name when --out is set.
    #[arg(long)]
    pub(crate) release_dir: PathBuf,

    /// Optional parent directory. When set, package output is --out/basename(--release-dir).
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,

    /// Stable release identifier written to manifest.json.
    #[arg(long, default_value = "local")]
    pub(crate) release_id: String,

    /// neige-app binary copied to bin/neige-app.
    #[arg(long)]
    pub(crate) app_bin: PathBuf,

    /// Optional root assets for inspecting or repackaging historical releases.
    #[arg(long)]
    pub(crate) web_dist: Option<PathBuf>,

    /// Next frontend copied to web/dist/next in the same Web release unit.
    #[arg(long)]
    pub(crate) fe_dist: Option<PathBuf>,

    /// Binary to copy into bin/, as NAME=PATH. Repeat for each bundle binary.
    #[arg(long = "bin", value_parser = package::parse_named_path)]
    pub(crate) bins: Vec<NamedPath>,
}

pub async fn run_cli() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,neige_app=debug")),
        )
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().command {
        CommandMode::System(SystemCommand::Setup(args)) => setup::run(args),
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
            let run_as = systemd_run_as_for_scope(
                cfg.systemd.scope,
                None,
                cfg.systemd.user.as_deref(),
                cfg.systemd.home.as_deref(),
            )?;
            print!(
                "{}",
                render_systemd_unit(
                    &name,
                    &bin,
                    &config_path,
                    &path_env,
                    cfg.systemd.scope,
                    run_as.as_ref(),
                )?
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

pub(crate) fn run_init_config(args: SystemInitConfigArgs) -> anyhow::Result<()> {
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

pub(crate) fn run_install(args: SystemInstallArgs) -> anyhow::Result<()> {
    let config_path = args.config.unwrap_or_else(default_config_path);
    let config_created = if config_path.exists() {
        false
    } else {
        init_config(&config_path)?;
        true
    };
    let cfg = AppConfig::load(Some(&config_path))?;
    ensure_system_install_permission(cfg.systemd.scope, &cfg.systemd.unit_path, &config_path)?;
    if let Some(parent) = cfg.systemd.unit_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            system_unit_write_error("create", parent, cfg.systemd.scope, &config_path, err)
        })?;
    }
    if cfg.systemd.unit_path.exists() && !args.force {
        anyhow::bail!(
            include_str!("../templates/system-install-conflict.md"),
            cfg.systemd.unit_path.display(),
            config_path.display(),
            cfg.systemd.unit_name,
            cfg.systemd.scope
        );
    }
    let path_env = resolve_systemd_path_env(args.path.as_deref())?;
    warn_missing_spawn_tools(&path_env);
    let run_as = systemd_run_as_for_scope(
        cfg.systemd.scope,
        args.user.as_deref(),
        cfg.systemd.user.as_deref(),
        cfg.systemd.home.as_deref(),
    )?;
    let token_created = ensure_admin_token_file(cfg.admin.token_file.as_ref())?;
    let unit = render_systemd_unit(
        &cfg.systemd.unit_name,
        &cfg.systemd.bin,
        &config_path,
        &path_env,
        cfg.systemd.scope,
        run_as.as_ref(),
    )?;
    std::fs::write(&cfg.systemd.unit_path, unit).map_err(|err| {
        system_unit_write_error(
            "write",
            &cfg.systemd.unit_path,
            cfg.systemd.scope,
            &config_path,
            err,
        )
    })?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "config": config_path,
            "configCreated": config_created,
            "tokenCreated": token_created,
            "unit": cfg.systemd.unit_path,
            "nextSteps": install_next_steps(cfg.systemd.scope, &cfg.systemd.unit_name),
        }))?
    );
    Ok(())
}

pub(crate) fn install_next_steps(scope: SystemdScope, unit_name: &str) -> Vec<String> {
    match scope {
        SystemdScope::User => vec![
            "systemctl --user daemon-reload".into(),
            format!("systemctl --user enable --now {unit_name}"),
        ],
        SystemdScope::System => vec![
            "systemctl daemon-reload".into(),
            format!("systemctl enable --now {unit_name}"),
        ],
    }
}

pub(crate) fn ensure_system_install_permission(
    scope: SystemdScope,
    unit_path: &Path,
    config_path: &Path,
) -> anyhow::Result<()> {
    if !matches!(scope, SystemdScope::System)
        || !unit_path.starts_with("/etc/systemd/system")
        || current_euid_is_root()
    {
        return Ok(());
    }
    anyhow::bail!(
        "installing a system-scope unit at {} (config {}) requires root; rerun the same invocation with appropriate permissions, preserving all arguments, or set systemd.unit_path to a writable test path",
        unit_path.display(),
        config_path.display()
    );
}

#[cfg(unix)]
pub(crate) fn current_euid_is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
pub(crate) fn current_euid_is_root() -> bool {
    false
}

pub(crate) fn system_unit_write_error(
    action: &str,
    path: &Path,
    scope: SystemdScope,
    config_path: &Path,
    err: std::io::Error,
) -> anyhow::Error {
    let advice = if err.kind() == std::io::ErrorKind::PermissionDenied {
        "; rerun the same invocation with appropriate permissions, preserving all arguments, or set systemd.unit_path to a writable test path"
    } else {
        ""
    };
    anyhow::Error::new(err).context(format!(
        "{action} {} (config {}, scope {:?}){advice}",
        path.display(),
        config_path.display(),
        scope
    ))
}

pub(crate) fn run_upgrade(args: SystemUpgradeArgs) -> anyhow::Result<()> {
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

pub(crate) fn upgrade_next_steps(
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
            systemctl_restart_step(cfg.systemd.scope, &cfg.systemd.unit_name),
        ],
        Some(_) => {
            vec!["No backend restart required; refresh open browser tabs or reload /calm/.".into()]
        }
        None => vec![
            "Review stage.preflight, then rerun with --activate to switch release symlinks.".into(),
        ],
    }
}

pub(crate) fn systemctl_restart_step(scope: SystemdScope, unit_name: &str) -> String {
    match scope {
        SystemdScope::User => format!("systemctl --user restart {unit_name}"),
        SystemdScope::System => format!("systemctl restart {unit_name}"),
    }
}

pub(crate) fn run_rollback(args: SystemRollbackArgs) -> anyhow::Result<()> {
    let cfg = AppConfig::load(args.config.as_deref())?;
    let result = upgrade::rollback_current(&cfg)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

pub(crate) fn run_preflight_cli(args: SystemPreflightArgs) -> anyhow::Result<()> {
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

pub(crate) fn run_package_cli(args: SystemPackageArgs) -> anyhow::Result<()> {
    let package_dir = package::build_package(&PackageConfig {
        release_dir: args.release_dir,
        out: args.out,
        release_id: args.release_id,
        app_bin: Some(args.app_bin),
        web_dist: args.web_dist,
        fe_dist: args.fe_dist,
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

pub(crate) fn read_json<T>(path: &PathBuf) -> anyhow::Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}
