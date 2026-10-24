//! Typed Planner startup and init contracts. Worker policy is supplied by WA2.
use super::spawn::{PRINT_MODE, PermissionRoot, TOOLS, allowed_tools, turn_settings_json};
use super::spawn::{ResumeTruncation, SessionStart, SpawnPurpose};
use crate::error::CalmError;
use crate::mcp_server::wiring::MCP_SERVER_KEY;
use crate::mcp_server::wiring::claude_mcp_config_json;
use crate::planner_permission_mode::PlannerPermissionMode;
use provider::TurnModelSelection;
use provider::claude::protocol::SystemInit;
use std::ffi::OsString;
use std::path::Path;
use uuid::Uuid;

/// The checks selected by the role of this process, independent of a Planner session.
pub(crate) trait ProtocolProfile: Send + Sync {
    fn thread(&self) -> Uuid;
    fn init_check(&self, init: &SystemInit) -> Result<(), String>;
}

pub(crate) struct PlannerRole {
    pub thread: Uuid,
    pub version: String,
    pub purpose: SpawnPurpose,
}
impl ProtocolProfile for PlannerRole {
    fn thread(&self) -> Uuid {
        self.thread
    }
    fn init_check(&self, init: &SystemInit) -> Result<(), String> {
        init_check(init, self)
    }
}

/// The Planner's existing argv contract, collected at the role boundary.
pub(crate) struct PlannerArgs<'a> {
    pub thread: Uuid,
    pub purpose: SpawnPurpose,
    pub start: SessionStart,
    pub truncation: Option<&'a ResumeTruncation>,
    pub selection: &'a TurnModelSelection,
    pub mode: PlannerPermissionMode,
    pub cwd: &'a Path,
    pub mcp_shim: &'a Path,
    pub instructions: &'a Path,
}

/// Configuration is selected explicitly; ambient CLAUDE_CONFIG_DIR is never inherited.
pub(crate) enum ConfigSource<'a> {
    #[allow(dead_code)] // Selected by WA2; WA1b only installs the Planner Dedicated profile.
    UserHome,
    Dedicated(&'a Path),
}

/// Planner's unchanged environment allowlist. WA2 supplies its own environment role.
pub(crate) struct PlannerEnvironment;
pub(crate) trait EnvironmentRole {
    fn passthrough_keys(&self) -> impl Iterator<Item = &'static str>;
}
impl EnvironmentRole for PlannerEnvironment {
    fn passthrough_keys(&self) -> impl Iterator<Item = &'static str> {
        super::spawn::passthrough_keys()
    }
}

/// The init checks of every spawn (§5.2), the second line of defence after `--version`; a
/// compaction makes its own command check in place of the no-skills one.
pub(crate) fn init_check(init: &SystemInit, profile: &PlannerRole) -> Result<(), String> {
    let PlannerRole {
        thread,
        version,
        purpose,
    } = profile;
    let thread = *thread;
    if init.claude_code_version != *version {
        return Err(format!(
            "claude reports version {}, the config pins {version}",
            init.claude_code_version
        ));
    }
    if init.session_id != thread {
        return Err(format!(
            "claude session {} is not thread {thread}",
            init.session_id
        ));
    }
    if !init
        .capabilities
        .iter()
        .any(|c| c == "interrupt_receipt_v1")
    {
        return Err("claude lacks the interrupt_receipt_v1 capability".into());
    }
    if !init
        .mcp_servers
        .iter()
        .any(|server| server.name == MCP_SERVER_KEY && server.status == "connected")
    {
        return Err("the neige MCP server is not connected".into());
    }
    match purpose {
        SpawnPurpose::Turn if !init.skills.is_empty() => {
            return Err(format!("claude loaded skills {:?}", init.skills));
        }
        SpawnPurpose::Turn => {}
        SpawnPurpose::Compaction => super::compact::init_check(init)?,
    }
    if let Some(plugin) = init
        .plugins
        .iter()
        .find(|p| !p.source.ends_with("@builtin"))
    {
        return Err(format!(
            "claude loaded a non-builtin plugin {}",
            plugin.source
        ));
    }
    Ok(())
}

/// Role-owned argv generation; the common spawn entry point does not infer role policy.
pub(crate) trait LaunchRole {
    fn argv(&self) -> crate::error::Result<Vec<std::ffi::OsString>>;
}

/// `selection` is the card's stored choice (#1822 6′): a model rides as `--model=<value>` and an
/// effort as `--effort=<level>`, one token each so a value can never be read as a flag; the CLI
/// judges the model and refuses one it cannot run. `None` passes the flag not at all, so the CLI
/// runs its default. `truncation` rides right after `--resume <thread>`; a new session has nothing
/// to cut. `mode` is the card's permission mode, read at this spawn (#2348); every mode loads the
/// project setting source, which is also where `CLAUDE.md` and `AGENTS.md` come from. A turn runs
/// with `--disable-slash-commands`; a compaction runs without it, since the flag disables every
/// command, built-in `/compact` included.
impl LaunchRole for PlannerArgs<'_> {
    fn argv(&self) -> crate::error::Result<Vec<std::ffi::OsString>> {
        let PlannerArgs {
            thread,
            purpose,
            start,
            truncation,
            selection,
            mode,
            cwd,
            mcp_shim,
            instructions,
        } = *self;
        let root = PermissionRoot::new(cwd)?;
        let session_flag = match start {
            SessionStart::New => "--session-id",
            SessionStart::Resume => "--resume",
        };
        let mut args: Vec<OsString> = PRINT_MODE.into_iter().map(OsString::from).collect();
        args.push("--replay-user-messages".into());
        args.push("--include-partial-messages".into());
        args.push(session_flag.into());
        args.push(thread.to_string().into());
        if let Some(truncation) = truncation {
            if start == SessionStart::New {
                return Err(CalmError::Conflict(
                    "a Claude conversation that has no session yet has no turn to remove".into(),
                ));
            }
            args.extend(truncation.flags());
        }
        if let Some(model) = &selection.model {
            args.push(format!("--model={model}").into());
        }
        if let Some(effort) = &selection.effort {
            args.push(format!("--effort={effort}").into());
        }
        args.push("--setting-sources".into());
        args.push("project".into());
        if purpose == SpawnPurpose::Turn {
            args.push("--disable-slash-commands".into());
        }
        for arg in ["--tools", TOOLS, "--strict-mcp-config"] {
            args.push(arg.into());
        }
        args.push("--mcp-config".into());
        args.push(claude_mcp_config_json(mcp_shim)?.into());
        args.push("--settings".into());
        args.push(turn_settings_json(mode, &root).into());
        match mode {
            PlannerPermissionMode::Never => {
                args.push("--permission-prompts".into());
                args.push("none".into());
            }
            PlannerPermissionMode::Ask => {
                args.push("--permission-prompt-tool".into());
                args.push("stdio".into());
            }
            // Every check is bypassed; whatever would still ask (a workspace's
            // `disableBypassPermissionsMode`) fails as under `never`.
            PlannerPermissionMode::Full => {
                args.push("--permission-mode".into());
                args.push("bypassPermissions".into());
                args.push("--permission-prompts".into());
                args.push("none".into());
            }
        }
        args.push("--allowedTools".into());
        args.push(allowed_tools(mode, &root).into());
        args.push("--append-system-prompt-file".into());
        args.push(instructions.as_os_str().to_owned());
        Ok(args)
    }
}
