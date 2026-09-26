//! The spawn contract of one Claude Planner turn (design #1791 §5.2, §5.3): argv, the allowlisted
//! environment, and the per-spawn instructions file.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use serde_json::json;
use uuid::Uuid;

use super::models::ClaudeModel;
use super::stop::MARKER_KEY;
use crate::error::{CalmError, Result};
use crate::shared_codex_appserver::SPAWN_ENV_PASSTHROUGH;

/// The CLI's own tools a Planner may use; everything else (Task, Skill, …) stays off.
const TOOLS: &str = "Bash,Read,Edit,Write,ToolSearch,WebFetch,WebSearch";

/// Owner decision (§9.6): the sandbox is always on and fails closed; the network is unrestricted.
pub(crate) fn settings_json() -> String {
    json!({
        "permissions": { "allow": ["WebFetch(domain:*)"] },
        "sandbox": {
            "enabled": true,
            "failIfUnavailable": true,
            "allowUnsandboxedCommands": false,
            "network": { "allowAllUnixSockets": true },
        },
    })
    .to_string()
}

/// The `calm` shim; its secrets reach it through `${VAR}` expansion of the CLI's own environment.
fn mcp_config_json(shim: &Path) -> Result<String> {
    let command = shim.to_str().ok_or_else(|| {
        CalmError::Internal(format!("mcp shim path is not UTF-8: {}", shim.display()))
    })?;
    Ok(json!({
        "mcpServers": {
            "calm": {
                "type": "stdio",
                "command": command,
                "args": [],
                "env": {
                    "NEIGE_MCP_SOCKET": "${NEIGE_MCP_SOCKET}",
                    "NEIGE_MCP_TOKEN": "${NEIGE_MCP_TOKEN}",
                },
            },
        },
    })
    .to_string())
}

/// `--allowedTools` rules. Edit and Write are confined to the workspace; the rule list is one
/// space-separated argument, so a workspace path that would split or unbalance a rule is refused.
fn allowed_tools(cwd: &Path) -> Result<String> {
    let cwd = cwd
        .to_str()
        .filter(|path| path.starts_with('/'))
        .filter(|path| {
            !path
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, ',' | '(' | ')'))
        })
        .ok_or_else(|| {
            CalmError::Conflict(format!(
                "workspace {} cannot be written as a Claude permission rule",
                cwd.display()
            ))
        })?;
    let root = cwd.trim_end_matches('/');
    Ok(format!(
        "Bash Read ToolSearch WebFetch WebSearch mcp__calm Edit(/{root}/**) Write(/{root}/**)"
    ))
}

/// Whether the thread's Claude session already exists (bound on its first `system/init`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStart {
    New,
    Resume,
}

/// `model` is the card's choice; `None` passes no `--model`, so the CLI runs its default.
pub(crate) fn argv(
    thread: Uuid,
    start: SessionStart,
    model: Option<&ClaudeModel>,
    cwd: &Path,
    mcp_shim: &Path,
    instructions: &Path,
) -> Result<Vec<OsString>> {
    let session_flag = match start {
        SessionStart::New => "--session-id",
        SessionStart::Resume => "--resume",
    };
    let mut args: Vec<OsString> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--replay-user-messages",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(session_flag.into());
    args.push(thread.to_string().into());
    if let Some(model) = model {
        args.push("--model".into());
        args.push(model.alias.into());
    }
    for arg in [
        "--setting-sources",
        "project",
        "--disable-slash-commands",
        "--tools",
        TOOLS,
        "--strict-mcp-config",
    ] {
        args.push(arg.into());
    }
    args.push("--mcp-config".into());
    args.push(mcp_config_json(mcp_shim)?.into());
    args.push("--settings".into());
    args.push(settings_json().into());
    args.push("--permission-prompts".into());
    args.push("none".into());
    args.push("--allowedTools".into());
    args.push(allowed_tools(cwd)?.into());
    args.push("--append-system-prompt-file".into());
    args.push(instructions.as_os_str().to_owned());
    Ok(args)
}

/// Ambient keys a Claude Planner inherits: the Codex daemon's list without the Codex, OpenAI and
/// Rust-diagnostics keys. Never an inherited `ANTHROPIC_*`, `CLAUDE_CODE_*` or `NEIGE_MCP_DAEMON_TOKEN`.
pub(crate) fn passthrough_keys() -> impl Iterator<Item = &'static str> {
    SPAWN_ENV_PASSTHROUGH.iter().copied().filter(|key| {
        !key.starts_with("OPENAI_")
            && !key.starts_with("CODEX_")
            && !key.starts_with("RUST_")
            && *key != "LOG_FORMAT"
    })
}

/// What the environment is computed from; the MCP token joins only the turn's own spawn.
pub(crate) struct EnvInputs<'a> {
    pub path: OsString,
    pub config_dir: &'a Path,
    pub mcp_socket: &'a Path,
    pub marker: String,
    /// `(UPPER, lower, value)` from the server's proxy resolver.
    pub proxy: &'a [(String, String, String)],
}

/// The `env_clear()` environment of the `--version` check: everything but the MCP token.
pub(crate) fn base_env(inputs: &EnvInputs<'_>) -> Vec<(String, OsString)> {
    let mut env: Vec<(String, OsString)> = passthrough_keys()
        .filter_map(|key| std::env::var_os(key).map(|value| (key.to_string(), value)))
        .collect();
    for (upper, lower, value) in inputs.proxy {
        env.push((upper.clone(), value.into()));
        env.push((lower.clone(), value.into()));
    }
    env.push(("PATH".into(), inputs.path.clone()));
    env.push((
        "CLAUDE_CONFIG_DIR".into(),
        inputs.config_dir.as_os_str().to_owned(),
    ));
    env.push((
        "NEIGE_MCP_SOCKET".into(),
        inputs.mcp_socket.as_os_str().to_owned(),
    ));
    env.push((MARKER_KEY.into(), inputs.marker.clone().into()));
    env.push(("DISABLE_AUTOUPDATER".into(), "1".into()));
    let (key, value) = crate::claude_code_env::DISABLE_AUTO_MEMORY;
    env.push((key.into(), value.into()));
    env
}

/// The per-spawn instructions file: 0600 in the 0700 `claude-planner/tmp` directory, kept off argv
/// because `/proc/<pid>/cmdline` is world-readable. Dropping the guard removes the file, so every
/// exit before `Ok` leaves none; settlement drops it after its `stop`.
#[derive(Debug)]
pub(crate) struct InstructionsFile {
    path: PathBuf,
}

impl InstructionsFile {
    pub(crate) fn write(
        dir: &Path,
        worker_session_id: &str,
        turn_id: &str,
        text: &str,
    ) -> Result<Self> {
        for part in [worker_session_id, turn_id] {
            if part.is_empty()
                || !part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(CalmError::BadRequest(format!(
                    "{part:?} cannot name a claude planner instructions file"
                )));
            }
        }
        let path = dir.join(format!("{worker_session_id}-{turn_id}.md"));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let guard = Self { path };
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        Ok(guard)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for InstructionsFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.path.display(),
                %error,
                "claude planner: could not remove an instructions file"
            );
        }
    }
}

/// `<data_dir>/claude-planner/tmp`, created 0700.
pub(crate) fn instructions_dir(data_dir: &Path) -> Result<PathBuf> {
    let dir = data_dir.join("claude-planner").join("tmp");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(dir)
}
