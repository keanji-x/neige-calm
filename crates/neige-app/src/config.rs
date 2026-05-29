use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, anyhow};

pub(crate) const DEFAULT_CONFIG_PATH: &str = "~/.config/neige-app/config.toml";

#[derive(Debug, Clone)]
pub(crate) struct AppConfig {
    pub config_path: PathBuf,
    pub admin: AdminConfig,
    pub release: ReleaseConfig,
    pub child: ChildConfig,
    pub timing: TimingConfig,
    pub systemd: SystemdConfig,
    pub upgrade: UpgradeConfig,
    pub source: SourceConfig,
}

#[derive(Debug, Clone)]
pub(crate) struct AdminConfig {
    pub listen: SocketAddr,
    pub token_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReleaseConfig {
    pub root: PathBuf,
    pub current: PathBuf,
    pub previous: PathBuf,
    pub current_server: PathBuf,
    pub current_web: PathBuf,
    pub previous_server: PathBuf,
    pub previous_web: PathBuf,
    pub backups: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct ChildConfig {
    pub bin: PathBuf,
    pub proc_supervisor_bin: PathBuf,
    pub web_dist: Option<PathBuf>,
    pub calm_listen: String,
    pub db_url: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub mcp_stdio_shim_bin: Option<PathBuf>,
    pub auth_username: Option<String>,
    pub auth_password: Option<String>,
    pub auth_dev_autologin: bool,
    pub cwd: Option<PathBuf>,
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct TimingConfig {
    pub stop_grace: Duration,
    pub restart_delay: Duration,
}

#[derive(Debug, Clone)]
pub(crate) struct SystemdConfig {
    pub unit_path: PathBuf,
    pub unit_name: String,
    pub bin: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct UpgradeConfig {
    pub current_version_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct SourceConfig {
    pub url: Option<String>,
    pub branch: String,
    pub mode: Option<crate::preflight::PreflightMode>,
    pub checkout_dir: PathBuf,
    pub build_args: Vec<String>,
    pub api_version: Option<String>,
    pub sync_event_version: Option<u32>,
    pub mcp_protocol_version: Option<String>,
    pub web_compat_version: Option<u32>,
    pub min_web_compat_version: Option<u32>,
    pub db_migration_policy: Option<crate::manifest::DbMigrationPolicy>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct ServeOverrides {
    pub admin_listen: Option<SocketAddr>,
    pub admin_token_file: Option<PathBuf>,
    pub child_bin: Option<PathBuf>,
    pub proc_supervisor_bin: Option<PathBuf>,
    pub calm_listen: Option<String>,
    pub calm_web_dist: Option<PathBuf>,
    pub calm_db_url: Option<String>,
    pub calm_data_dir: Option<PathBuf>,
    pub calm_mcp_stdio_shim_bin: Option<PathBuf>,
    pub child_cwd: Option<PathBuf>,
    pub child_args: Option<Vec<String>>,
    pub restart_delay_ms: Option<u64>,
    pub stop_grace_ms: Option<u64>,
}

#[derive(Debug, Default)]
struct ConfigBuilder {
    admin_listen: Option<String>,
    admin_token_file: Option<String>,
    release_root: Option<String>,
    release_current: Option<String>,
    release_previous: Option<String>,
    release_current_server: Option<String>,
    release_current_web: Option<String>,
    release_previous_server: Option<String>,
    release_previous_web: Option<String>,
    release_backups: Option<String>,
    child_bin: Option<String>,
    child_proc_supervisor_bin: Option<String>,
    child_web_dist: Option<String>,
    child_calm_listen: Option<String>,
    child_db_url: Option<String>,
    child_data_dir: Option<String>,
    child_mcp_stdio_shim_bin: Option<String>,
    child_auth_username: Option<String>,
    child_auth_password: Option<String>,
    child_auth_dev_autologin: Option<bool>,
    child_cwd: Option<String>,
    child_extra_args: Option<Vec<String>>,
    timing_stop_grace_ms: Option<u64>,
    timing_restart_delay_ms: Option<u64>,
    systemd_unit_path: Option<String>,
    systemd_unit_name: Option<String>,
    systemd_bin: Option<String>,
    upgrade_current_version_file: Option<String>,
    source_url: Option<String>,
    source_branch: Option<String>,
    source_mode: Option<String>,
    source_checkout_dir: Option<String>,
    source_build_args: Option<Vec<String>>,
    source_api_version: Option<String>,
    source_sync_event_version: Option<u32>,
    source_mcp_protocol_version: Option<String>,
    source_web_compat_version: Option<u32>,
    source_min_web_compat_version: Option<u32>,
    source_db_migration_policy: Option<String>,
}

impl AppConfig {
    pub(crate) fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let explicit = path.is_some();
        let path = path.map(PathBuf::from).unwrap_or_else(default_config_path);
        if !path.exists() {
            if explicit {
                return Err(anyhow!("config {} does not exist", path.display()));
            }
            return Ok(Self::starter(path));
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("read config {}", path.display()))?;
        let builder = parse_config(&text)?;
        Self::from_builder(path, builder)
    }

    pub(crate) fn starter(path: PathBuf) -> Self {
        let release_root = expand_tilde("~/.local/share/neige-app/releases");
        let current_server = release_root.join("current-server");
        let current_web = release_root.join("current-web");
        let previous_server = release_root.join("previous-server");
        Self {
            config_path: path,
            admin: AdminConfig {
                listen: "127.0.0.1:4050".parse().expect("valid default listen"),
                token_file: Some(expand_tilde("~/.config/neige-app/admin.token")),
            },
            release: ReleaseConfig {
                root: release_root.clone(),
                current: current_server.clone(),
                previous: previous_server.clone(),
                current_server: current_server.clone(),
                current_web: current_web.clone(),
                previous_server,
                previous_web: release_root.join("previous-web"),
                backups: expand_tilde("~/.local/share/neige-app/backups"),
            },
            child: ChildConfig {
                bin: current_server.join("bin").join("calm-server"),
                proc_supervisor_bin: current_server.join("bin").join("calm-proc-supervisor"),
                web_dist: Some(current_web.join("web").join("dist")),
                calm_listen: "127.0.0.1:4040".into(),
                db_url: None,
                data_dir: Some(expand_tilde("~/.local/share/neige-calm")),
                mcp_stdio_shim_bin: Some(current_server.join("bin").join("neige-mcp-stdio-shim")),
                auth_username: Some("owner".into()),
                auth_password: None,
                auth_dev_autologin: false,
                cwd: None,
                extra_args: Vec::new(),
            },
            timing: TimingConfig {
                stop_grace: Duration::from_millis(5000),
                restart_delay: Duration::from_millis(1000),
            },
            systemd: SystemdConfig {
                unit_path: expand_tilde("~/.config/systemd/user/neige-app.service"),
                unit_name: "neige-app".into(),
                bin: PathBuf::from("/usr/local/bin/neige-app"),
            },
            upgrade: UpgradeConfig {
                current_version_file: None,
            },
            source: SourceConfig {
                url: None,
                branch: "main".into(),
                mode: None,
                checkout_dir: expand_tilde("~/.cache/neige-app/source"),
                build_args: vec!["make".into(), "build".into()],
                api_version: None,
                sync_event_version: None,
                mcp_protocol_version: None,
                web_compat_version: None,
                min_web_compat_version: None,
                db_migration_policy: None,
            },
        }
    }

    fn from_builder(path: PathBuf, builder: ConfigBuilder) -> anyhow::Result<Self> {
        let mut cfg = Self::starter(path);
        if let Some(value) = builder.admin_listen {
            cfg.admin.listen = value
                .parse()
                .with_context(|| format!("parse admin.listen {value}"))?;
        }
        cfg.admin.token_file = builder.admin_token_file.map(|v| expand_tilde(&v));
        if let Some(value) = builder.release_root {
            cfg.release.root = expand_tilde(&value);
        }
        let legacy_current = builder.release_current.map(|v| expand_tilde(&v));
        let legacy_previous = builder.release_previous.map(|v| expand_tilde(&v));
        cfg.release.current_server = builder
            .release_current_server
            .map(|v| expand_tilde(&v))
            .unwrap_or_else(|| cfg.release.root.join("current-server"));
        cfg.release.current_web = builder
            .release_current_web
            .map(|v| expand_tilde(&v))
            .unwrap_or_else(|| cfg.release.root.join("current-web"));
        cfg.release.previous_server = builder
            .release_previous_server
            .map(|v| expand_tilde(&v))
            .unwrap_or_else(|| cfg.release.root.join("previous-server"));
        cfg.release.previous_web = builder
            .release_previous_web
            .map(|v| expand_tilde(&v))
            .unwrap_or_else(|| cfg.release.root.join("previous-web"));
        cfg.release.current = legacy_current.unwrap_or_else(|| cfg.release.current_server.clone());
        cfg.release.previous =
            legacy_previous.unwrap_or_else(|| cfg.release.previous_server.clone());
        cfg.release.backups = builder
            .release_backups
            .map(|v| expand_tilde(&v))
            .unwrap_or_else(|| expand_tilde("~/.local/share/neige-app/backups"));

        cfg.child.bin = builder
            .child_bin
            .map(|v| expand_tilde(&v))
            .unwrap_or_else(|| cfg.release.current_server.join("bin").join("calm-server"));
        cfg.child.proc_supervisor_bin = builder
            .child_proc_supervisor_bin
            .map(|v| expand_tilde(&v))
            .unwrap_or_else(|| {
                cfg.release
                    .current_server
                    .join("bin")
                    .join("calm-proc-supervisor")
            });
        cfg.child.web_dist = builder
            .child_web_dist
            .map(|v| expand_tilde(&v))
            .or_else(|| Some(cfg.release.current_web.join("web").join("dist")));
        if let Some(value) = builder.child_calm_listen {
            cfg.child.calm_listen = value;
        }
        cfg.child.db_url = builder.child_db_url;
        cfg.child.data_dir = builder.child_data_dir.map(|v| expand_tilde(&v));
        cfg.child.mcp_stdio_shim_bin = builder
            .child_mcp_stdio_shim_bin
            .map(|v| expand_tilde(&v))
            .or_else(|| {
                Some(
                    cfg.release
                        .current_server
                        .join("bin")
                        .join("neige-mcp-stdio-shim"),
                )
            });
        cfg.child.auth_username = builder.child_auth_username;
        cfg.child.auth_password = builder.child_auth_password;
        if let Some(value) = builder.child_auth_dev_autologin {
            cfg.child.auth_dev_autologin = value;
        }
        cfg.child.cwd = builder.child_cwd.map(|v| expand_tilde(&v));
        if let Some(value) = builder.child_extra_args {
            cfg.child.extra_args = value;
        }
        if let Some(value) = builder.timing_stop_grace_ms {
            cfg.timing.stop_grace = Duration::from_millis(value);
        }
        if let Some(value) = builder.timing_restart_delay_ms {
            cfg.timing.restart_delay = Duration::from_millis(value);
        }
        if let Some(value) = builder.systemd_unit_path {
            cfg.systemd.unit_path = expand_tilde(&value);
        }
        if let Some(value) = builder.systemd_unit_name {
            cfg.systemd.unit_name = value;
        }
        if let Some(value) = builder.systemd_bin {
            cfg.systemd.bin = expand_tilde(&value);
        }
        cfg.upgrade.current_version_file = builder
            .upgrade_current_version_file
            .map(|v| expand_tilde(&v));
        cfg.source.url = builder.source_url;
        if let Some(value) = builder.source_branch {
            cfg.source.branch = value;
        }
        if let Some(value) = builder.source_mode {
            cfg.source.mode = Some(
                parse_source_mode(&value).with_context(|| format!("parse source.mode {value}"))?,
            );
        }
        if let Some(value) = builder.source_checkout_dir {
            cfg.source.checkout_dir = expand_tilde(&value);
        }
        if let Some(value) = builder.source_build_args {
            cfg.source.build_args = value;
        }
        if let Some(value) = builder.source_api_version {
            cfg.source.api_version = Some(value);
        }
        if let Some(value) = builder.source_sync_event_version {
            cfg.source.sync_event_version = Some(value);
        }
        if let Some(value) = builder.source_mcp_protocol_version {
            cfg.source.mcp_protocol_version = Some(value);
        }
        if let Some(value) = builder.source_web_compat_version {
            cfg.source.web_compat_version = Some(value);
        }
        if let Some(value) = builder.source_min_web_compat_version {
            cfg.source.min_web_compat_version = Some(value);
        }
        if let Some(value) = builder.source_db_migration_policy {
            cfg.source.db_migration_policy = Some(
                parse_db_migration_policy(&value)
                    .with_context(|| format!("parse source.db_migration_policy {value}"))?,
            );
        }
        Ok(cfg)
    }

    pub(crate) fn apply_serve_overrides(&mut self, overrides: ServeOverrides) {
        if let Some(value) = overrides.admin_listen {
            self.admin.listen = value;
        }
        if let Some(value) = overrides.admin_token_file {
            self.admin.token_file = Some(value);
        }
        if let Some(value) = overrides.child_bin {
            self.child.bin = value;
        }
        if let Some(value) = overrides.proc_supervisor_bin {
            self.child.proc_supervisor_bin = value;
        }
        if let Some(value) = overrides.calm_listen {
            self.child.calm_listen = value;
        }
        if let Some(value) = overrides.calm_web_dist {
            self.child.web_dist = Some(value);
        }
        if let Some(value) = overrides.calm_db_url {
            self.child.db_url = Some(value);
        }
        if let Some(value) = overrides.calm_data_dir {
            self.child.data_dir = Some(value);
        }
        if let Some(value) = overrides.calm_mcp_stdio_shim_bin {
            self.child.mcp_stdio_shim_bin = Some(value);
        }
        if let Some(value) = overrides.child_cwd {
            self.child.cwd = Some(value);
        }
        if let Some(value) = overrides.child_args {
            self.child.extra_args = value;
        }
        if let Some(value) = overrides.restart_delay_ms {
            self.timing.restart_delay = Duration::from_millis(value);
        }
        if let Some(value) = overrides.stop_grace_ms {
            self.timing.stop_grace = Duration::from_millis(value);
        }
    }

    pub(crate) fn calm_data_dir_resolved(&self) -> PathBuf {
        self.child
            .data_dir
            .clone()
            .unwrap_or_else(|| expand_tilde("~/.local/share/neige-calm"))
    }

    pub(crate) fn proc_supervisor_sock(&self) -> PathBuf {
        self.calm_data_dir_resolved().join("proc-supervisor.sock")
    }
}

pub(crate) fn default_config_path() -> PathBuf {
    expand_tilde(DEFAULT_CONFIG_PATH)
}

pub(crate) fn init_config(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        return Err(anyhow!("config {} already exists", path.display()));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(path, starter_config_text()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub(crate) fn starter_config_text() -> &'static str {
    r#"# neige-app system-only configuration

[admin]
listen = "127.0.0.1:4050"
token_file = "~/.config/neige-app/admin.token"

[release]
root = "~/.local/share/neige-app/releases"
current_server = "~/.local/share/neige-app/releases/current-server"
current_web = "~/.local/share/neige-app/releases/current-web"
previous_server = "~/.local/share/neige-app/releases/previous-server"
previous_web = "~/.local/share/neige-app/releases/previous-web"
backups = "~/.local/share/neige-app/backups"

[child]
bin = "~/.local/share/neige-app/releases/current-server/bin/calm-server"
proc_supervisor_bin = "~/.local/share/neige-app/releases/current-server/bin/calm-proc-supervisor"
web_dist = "~/.local/share/neige-app/releases/current-web/web/dist"
calm_listen = "127.0.0.1:4040"
db_url = ""
data_dir = "~/.local/share/neige-calm"
mcp_stdio_shim_bin = "~/.local/share/neige-app/releases/current-server/bin/neige-mcp-stdio-shim"
auth_username = "owner"
auth_password = ""
auth_dev_autologin = false
cwd = ""
extra_args = []

[timing]
stop_grace_ms = 5000
restart_delay_ms = 1000

[systemd]
unit_path = "~/.config/systemd/user/neige-app.service"
unit_name = "neige-app"
bin = "/usr/local/bin/neige-app"

[upgrade]
current_version_file = ""

[source]
url = ""
branch = "main"
# Optional: web-only, server-only, or bundle. Omit to infer from manifest.
mode = ""
checkout_dir = "~/.cache/neige-app/source"
build_args = ["make", "build"]
# Source-driven upgrades fail closed unless these are explicitly configured.
# api_version = "1"
# sync_event_version = 1
# mcp_protocol_version = "2025-11-25"
# web_compat_version = 2
# min_web_compat_version = 2
# db_migration_policy = "forwardOnly"
"#
}

pub(crate) fn expand_tilde(value: &str) -> PathBuf {
    if value == "~" {
        return home_dir();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(value)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn parse_config(text: &str) -> anyhow::Result<ConfigBuilder> {
    let mut builder = ConfigBuilder::default();
    let mut section = String::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name.trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(anyhow!("invalid config line {}: {line}", index + 1));
        };
        set_value(&mut builder, &section, key.trim(), value.trim())
            .with_context(|| format!("parse config line {}", index + 1))?;
    }
    Ok(builder)
}

fn set_value(
    builder: &mut ConfigBuilder,
    section: &str,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    match (section, key) {
        ("admin", "listen") => builder.admin_listen = Some(parse_string(value)?),
        ("admin", "token_file") => builder.admin_token_file = parse_optional_string(value)?,
        ("release", "root") => builder.release_root = Some(parse_string(value)?),
        ("release", "current") => builder.release_current = Some(parse_string(value)?),
        ("release", "previous") => builder.release_previous = Some(parse_string(value)?),
        ("release", "current_server") => {
            builder.release_current_server = Some(parse_string(value)?)
        }
        ("release", "current_web") => builder.release_current_web = Some(parse_string(value)?),
        ("release", "previous_server") => {
            builder.release_previous_server = Some(parse_string(value)?)
        }
        ("release", "previous_web") => builder.release_previous_web = Some(parse_string(value)?),
        ("release", "backups") => builder.release_backups = Some(parse_string(value)?),
        ("child", "bin") => builder.child_bin = Some(parse_string(value)?),
        ("child", "proc_supervisor_bin") => {
            builder.child_proc_supervisor_bin = Some(parse_string(value)?)
        }
        ("child", "web_dist") => builder.child_web_dist = parse_optional_string(value)?,
        ("child", "calm_listen") => builder.child_calm_listen = Some(parse_string(value)?),
        ("child", "db_url") => builder.child_db_url = parse_optional_string(value)?,
        ("child", "data_dir") => builder.child_data_dir = parse_optional_string(value)?,
        ("child", "mcp_stdio_shim_bin") => {
            builder.child_mcp_stdio_shim_bin = parse_optional_string(value)?
        }
        ("child", "auth_username") => builder.child_auth_username = parse_optional_string(value)?,
        ("child", "auth_password") => builder.child_auth_password = parse_optional_string(value)?,
        ("child", "auth_dev_autologin") => {
            builder.child_auth_dev_autologin = Some(parse_bool(value)?)
        }
        ("child", "cwd") => builder.child_cwd = parse_optional_string(value)?,
        ("child", "extra_args") => builder.child_extra_args = Some(parse_string_array(value)?),
        ("timing", "stop_grace_ms") => builder.timing_stop_grace_ms = Some(parse_u64(value)?),
        ("timing", "restart_delay_ms") => builder.timing_restart_delay_ms = Some(parse_u64(value)?),
        ("systemd", "unit_path") => builder.systemd_unit_path = Some(parse_string(value)?),
        ("systemd", "unit_name") => builder.systemd_unit_name = Some(parse_string(value)?),
        ("systemd", "bin") => builder.systemd_bin = Some(parse_string(value)?),
        ("upgrade", "current_version_file") => {
            builder.upgrade_current_version_file = parse_optional_string(value)?
        }
        ("source", "url") => builder.source_url = parse_optional_string(value)?,
        ("source", "branch") => builder.source_branch = Some(parse_string(value)?),
        ("source", "mode") => builder.source_mode = parse_optional_string(value)?,
        ("source", "checkout_dir") => builder.source_checkout_dir = Some(parse_string(value)?),
        ("source", "build_args") => builder.source_build_args = Some(parse_string_array(value)?),
        ("source", "api_version") => builder.source_api_version = Some(parse_string(value)?),
        ("source", "sync_event_version") => {
            builder.source_sync_event_version = Some(parse_u32(value)?)
        }
        ("source", "mcp_protocol_version") => {
            builder.source_mcp_protocol_version = Some(parse_string(value)?)
        }
        ("source", "web_compat_version") => {
            builder.source_web_compat_version = Some(parse_u32(value)?)
        }
        ("source", "min_web_compat_version") => {
            builder.source_min_web_compat_version = Some(parse_u32(value)?)
        }
        ("source", "db_migration_policy") => {
            builder.source_db_migration_policy = Some(parse_string(value)?)
        }
        _ => {}
    }
    Ok(())
}

fn parse_string(value: &str) -> anyhow::Result<String> {
    let value = strip_comment(value).trim();
    let Some(inner) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return Err(anyhow!("expected quoted string"));
    };
    Ok(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn parse_optional_string(value: &str) -> anyhow::Result<Option<String>> {
    let value = parse_string(value)?;
    Ok(if value.is_empty() { None } else { Some(value) })
}

fn parse_u64(value: &str) -> anyhow::Result<u64> {
    Ok(strip_comment(value).trim().parse()?)
}

fn parse_u32(value: &str) -> anyhow::Result<u32> {
    Ok(strip_comment(value).trim().parse()?)
}

fn parse_bool(value: &str) -> anyhow::Result<bool> {
    match strip_comment(value).trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(anyhow!("expected boolean true or false, got {other}")),
    }
}

fn parse_db_migration_policy(value: &str) -> anyhow::Result<crate::manifest::DbMigrationPolicy> {
    match value {
        "none" => Ok(crate::manifest::DbMigrationPolicy::None),
        "additive" => Ok(crate::manifest::DbMigrationPolicy::Additive),
        "forwardOnly" => Ok(crate::manifest::DbMigrationPolicy::ForwardOnly),
        "destructive" => Ok(crate::manifest::DbMigrationPolicy::Destructive),
        _ => Err(anyhow!(
            "expected one of: none, additive, forwardOnly, destructive"
        )),
    }
}

fn parse_source_mode(value: &str) -> anyhow::Result<crate::preflight::PreflightMode> {
    match value {
        "web-only" => Ok(crate::preflight::PreflightMode::WebOnly),
        "server-only" => Ok(crate::preflight::PreflightMode::ServerOnly),
        "bundle" => Ok(crate::preflight::PreflightMode::Bundle),
        _ => Err(anyhow!("expected one of: web-only, server-only, bundle")),
    }
}

fn parse_string_array(value: &str) -> anyhow::Result<Vec<String>> {
    let value = strip_comment(value).trim();
    if value == "[]" {
        return Ok(Vec::new());
    }
    let Some(inner) = value.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return Err(anyhow!("expected string array"));
    };
    let mut out = Vec::new();
    for item in inner.split(',') {
        let item = item.trim();
        if !item.is_empty() {
            out.push(parse_string(item)?);
        }
    }
    Ok(out)
}

fn strip_comment(value: &str) -> &str {
    value
        .split_once(" #")
        .map(|(left, _)| left)
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_have_expected_paths() {
        let cfg = AppConfig::starter(PathBuf::from("/tmp/config.toml"));
        assert_eq!(cfg.admin.listen.to_string(), "127.0.0.1:4050");
        assert!(cfg.child.bin.ends_with("current-server/bin/calm-server"));
        assert!(
            cfg.child
                .web_dist
                .as_ref()
                .expect("web dist")
                .ends_with("current-web/web/dist")
        );
        assert!(
            cfg.child
                .mcp_stdio_shim_bin
                .as_ref()
                .expect("shim")
                .ends_with("current-server/bin/neige-mcp-stdio-shim")
        );
        assert!(cfg.systemd.unit_path.ends_with("neige-app.service"));
    }

    #[test]
    fn legacy_current_previous_do_not_alias_split_release_paths() {
        let tmp = test_temp_dir("config-legacy-release");
        let path = tmp.join("config.toml");
        fs::write(
            &path,
            r#"
[release]
root = "/releases"
current = "/releases/current"
previous = "/releases/previous"
"#,
        )
        .expect("write config");

        let cfg = AppConfig::load(Some(&path)).expect("load config");

        assert_eq!(
            cfg.release.current_server,
            PathBuf::from("/releases/current-server")
        );
        assert_eq!(
            cfg.release.current_web,
            PathBuf::from("/releases/current-web")
        );
        assert_eq!(
            cfg.release.previous_server,
            PathBuf::from("/releases/previous-server")
        );
        assert_eq!(
            cfg.release.previous_web,
            PathBuf::from("/releases/previous-web")
        );
        assert_eq!(cfg.release.current, PathBuf::from("/releases/current"));
        assert_eq!(cfg.release.previous, PathBuf::from("/releases/previous"));
        assert_eq!(
            cfg.child.bin,
            PathBuf::from("/releases/current-server/bin/calm-server")
        );
        assert_eq!(
            cfg.child.web_dist,
            Some(PathBuf::from("/releases/current-web/web/dist"))
        );
    }

    #[test]
    fn config_loads_values_and_applies_cli_overlay() {
        let tmp = test_temp_dir("config-load");
        let path = tmp.join("config.toml");
        fs::write(
            &path,
            r#"
[admin]
listen = "127.0.0.1:5000"
token_file = "/tmp/token"

[child]
bin = "/opt/neige/calm-server"
calm_listen = "127.0.0.1:5001"
auth_username = "admin"
auth_password = "secret"
auth_dev_autologin = true
extra_args = ["--one", "two"]

[timing]
restart_delay_ms = 250
"#,
        )
        .expect("write config");

        let mut cfg = AppConfig::load(Some(&path)).expect("load config");
        assert_eq!(cfg.admin.listen.to_string(), "127.0.0.1:5000");
        assert_eq!(cfg.child.auth_username.as_deref(), Some("admin"));
        assert_eq!(cfg.child.auth_password.as_deref(), Some("secret"));
        assert!(cfg.child.auth_dev_autologin);
        assert_eq!(cfg.child.extra_args, vec!["--one", "two"]);
        assert_eq!(cfg.timing.restart_delay, Duration::from_millis(250));

        cfg.apply_serve_overrides(ServeOverrides {
            calm_listen: Some("127.0.0.1:6001".into()),
            restart_delay_ms: Some(750),
            ..ServeOverrides::default()
        });
        assert_eq!(cfg.child.calm_listen, "127.0.0.1:6001");
        assert_eq!(cfg.timing.restart_delay, Duration::from_millis(750));
    }

    #[test]
    fn config_loads_source_settings() {
        let tmp = test_temp_dir("config-source");
        let path = tmp.join("config.toml");
        fs::write(
            &path,
            r#"
[source]
url = "/repo"
branch = "release"
mode = "web-only"
checkout_dir = "/checkout"
build_args = ["make", "build"]
api_version = "2"
sync_event_version = 3
mcp_protocol_version = "2026-01-01"
web_compat_version = 4
min_web_compat_version = 5
db_migration_policy = "additive"
"#,
        )
        .expect("write config");

        let cfg = AppConfig::load(Some(&path)).expect("load config");
        assert_eq!(cfg.source.url.as_deref(), Some("/repo"));
        assert_eq!(cfg.source.branch, "release");
        assert_eq!(
            cfg.source.mode,
            Some(crate::preflight::PreflightMode::WebOnly)
        );
        assert_eq!(cfg.source.checkout_dir, PathBuf::from("/checkout"));
        assert_eq!(cfg.source.build_args, vec!["make", "build"]);
        assert_eq!(cfg.source.sync_event_version, Some(3));
        assert_eq!(
            cfg.source.db_migration_policy,
            Some(crate::manifest::DbMigrationPolicy::Additive)
        );
    }

    #[test]
    fn init_config_refuses_to_overwrite() {
        let tmp = test_temp_dir("init-config");
        let path = tmp.join("config.toml");
        init_config(&path).expect("first init");
        let err = init_config(&path).expect_err("second init must fail");
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn explicit_missing_config_fails() {
        let tmp = test_temp_dir("missing-config");
        let path = tmp.join("missing.toml");
        let err = AppConfig::load(Some(&path)).expect_err("explicit missing config must fail");
        assert!(err.to_string().contains("does not exist"));
    }

    fn test_temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("neige-app-{name}-{}", std::process::id()));
        if path.exists() {
            fs::remove_dir_all(&path).expect("remove stale temp dir");
        }
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }
}
