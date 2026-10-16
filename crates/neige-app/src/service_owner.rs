//! Declared systemd installation/rendering and owned local credential-file setup.
use crate::config::SystemdScope;
use anyhow::Context;
use std::fs::OpenOptions;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SystemdRunAs {
    pub(crate) user: String,
    pub(crate) home: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SystemdInstallEnv {
    pub(crate) sudo_user: Option<String>,
    pub(crate) user: Option<String>,
}

impl SystemdInstallEnv {
    pub(crate) fn from_process() -> Self {
        Self {
            sudo_user: std::env::var("SUDO_USER")
                .ok()
                .filter(|value| !value.is_empty()),
            user: std::env::var("USER").ok().filter(|value| !value.is_empty()),
        }
    }
}

pub(crate) fn systemd_run_as_for_scope(
    scope: SystemdScope,
    cli_user: Option<&str>,
    config_user: Option<&str>,
    config_home: Option<&Path>,
) -> anyhow::Result<Option<SystemdRunAs>> {
    match scope {
        SystemdScope::User => Ok(None),
        SystemdScope::System => resolve_systemd_run_as(
            cli_user,
            config_user,
            config_home,
            &SystemdInstallEnv::from_process(),
            Path::new("/etc/passwd"),
        )
        .map(Some),
    }
}

pub(crate) fn resolve_systemd_run_as(
    cli_user: Option<&str>,
    config_user: Option<&str>,
    config_home: Option<&Path>,
    env: &SystemdInstallEnv,
    passwd_path: &Path,
) -> anyhow::Result<SystemdRunAs> {
    let explicit_user = cli_user
        .filter(|value| !value.is_empty())
        .or_else(|| config_user.filter(|value| !value.is_empty()));
    let user = explicit_user
        .or_else(|| env.sudo_user.as_deref().filter(|value| !value.is_empty()))
        .or_else(|| env.user.as_deref().filter(|value| !value.is_empty()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "systemd system scope requires a run user; set systemd.user or pass --user"
            )
        })?;
    if user == "root" && explicit_user.is_none() {
        anyhow::bail!(
            "refusing to install a system unit as root inferred from the environment; set systemd.user or pass --user"
        );
    }
    validate_systemd_identity(user)?;
    let home = match config_home {
        Some(home) => home.to_path_buf(),
        None => passwd_home_for_user(passwd_path, user)?.with_context(|| {
            format!(
                "resolve home for user {user} from {}",
                passwd_path.display()
            )
        })?,
    };
    validate_systemd_home(&home)?;
    Ok(SystemdRunAs {
        user: user.to_owned(),
        home,
    })
}

pub(crate) fn passwd_home_for_user(
    passwd_path: &Path,
    user: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let text = std::fs::read_to_string(passwd_path)
        .with_context(|| format!("read {}", passwd_path.display()))?;
    for line in text.lines() {
        let mut fields = line.split(':');
        let Some(name) = fields.next() else {
            continue;
        };
        if name != user {
            continue;
        }
        let home = fields.nth(4).unwrap_or_default();
        if home.is_empty() {
            return Ok(None);
        }
        return Ok(Some(PathBuf::from(home)));
    }
    Ok(None)
}

pub(crate) fn render_systemd_unit(
    name: &str,
    bin: &Path,
    config_path: &Path,
    path_env: &str,
    scope: SystemdScope,
    run_as: Option<&SystemdRunAs>,
) -> anyhow::Result<String> {
    validate_systemd_exec_path(bin, "systemd.bin")?;
    validate_systemd_exec_path(config_path, "config path")?;
    validate_systemd_path_env(path_env)?;
    let escaped_path_env = escape_systemd_environment_value(path_env);
    match scope {
        SystemdScope::User => Ok(format!(
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
        )),
        SystemdScope::System => {
            let run_as = run_as.context("system systemd unit rendering requires user and home")?;
            validate_systemd_identity(&run_as.user)?;
            validate_systemd_home(&run_as.home)?;
            Ok(format!(
                "\
[Unit]
Description={name} system service
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User={user}
Group={user}
Environment=\"PATH={escaped_path_env}\"
Environment=HOME={home}
ExecStart={bin} system serve --config {config_path}
Restart=always
RestartSec=2
Delegate=yes
KillMode=mixed

[Install]
WantedBy=multi-user.target
",
                name = name,
                user = run_as.user,
                home = run_as.home.display(),
                bin = bin.display(),
                config_path = config_path.display(),
                escaped_path_env = escaped_path_env,
            ))
        }
    }
}

pub(crate) fn resolve_systemd_path_env(override_path: Option<&str>) -> anyhow::Result<String> {
    let path_env = override_path
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
    validate_systemd_path_env(&path_env)?;
    Ok(path_env)
}

pub(crate) fn validate_systemd_path_env(path_env: &str) -> anyhow::Result<()> {
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

pub(crate) fn validate_systemd_identity(value: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        anyhow::bail!("systemd user must not be empty");
    }
    if value
        .chars()
        .any(|c| c.is_ascii_control() || c.is_ascii_whitespace())
    {
        anyhow::bail!("systemd user must not contain whitespace or control characters");
    }
    if value.contains('%') {
        anyhow::bail!("systemd user must not contain % systemd specifiers");
    }
    Ok(())
}

pub(crate) fn validate_systemd_home(home: &Path) -> anyhow::Result<()> {
    let value = home.as_os_str().to_string_lossy();
    if value.is_empty() {
        anyhow::bail!("systemd home must not be empty");
    }
    if value
        .chars()
        .any(|c| c.is_ascii_control() || c.is_ascii_whitespace())
    {
        anyhow::bail!("systemd home must not contain whitespace or control characters");
    }
    if value.contains('%') {
        anyhow::bail!("systemd home must not contain % systemd specifiers");
    }
    Ok(())
}

pub(crate) fn escape_systemd_environment_value(value: &str) -> String {
    // systemd.exec(5) Environment= takes a quoted, C-style-escaped value. "$" is literal in Environment= (no variable expansion), only specifier "%" expansion happens — and we reject "%" upstream.
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn warn_missing_spawn_tools(path_env: &str) {
    for tool in ["codex", "claude", "git"] {
        if !path_has_executable(path_env, tool) {
            eprintln!(
                "warning: {tool} not found on PATH ({path_env}); tracks using {tool} will fail to start"
            );
        }
    }
}

pub(crate) fn path_has_executable(path_env: &str, tool: &str) -> bool {
    path_env
        .split(':')
        .map(|dir| Path::new(dir).join(tool))
        .any(|candidate| is_executable_file(&candidate))
}

#[cfg(unix)]
pub(crate) fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
pub(crate) fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

pub(crate) fn validate_systemd_exec_path(path: &Path, label: &str) -> anyhow::Result<()> {
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

pub(crate) fn ensure_admin_token_file(path: Option<&PathBuf>) -> anyhow::Result<bool> {
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
