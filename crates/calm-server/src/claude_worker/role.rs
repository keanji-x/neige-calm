//! The worker's protocol profile and argv (§3.4): the Planner's session checks, none of its
//! tool, setting-source or plugin policy.
use crate::claude_planner::role::{LaunchRole, ProtocolProfile, session_checks};
use crate::claude_planner::spawn::stream_json_args;
use crate::error::Result;
use crate::mcp_server::wiring::{MCP_SERVER_KEY, claude_mcp_config_json};
use crate::operation::claude_adapter::CLAUDE_WORKER_PERMISSION_FLAGS;
use provider::claude::protocol::SystemInit;
use std::ffi::OsString;
use std::path::Path;
use uuid::Uuid;

/// What a worker turn's `init` must show (§3.4): its own session, the interrupt receipt and a
/// connected kernel MCP server. The owner's skills and plugins are the worker's own (OQ4, WA0 E6),
/// and the version is recorded through the turn port, never pinned (§3.4.1, K4). A permission
/// request the CLI still sends is denied back to the model by the sink (OQ3).
pub(crate) type WorkerRole = WorkerStart;
impl ProtocolProfile for WorkerStart {
    fn thread(&self) -> Uuid {
        WorkerStart::thread(self)
    }
    fn init_check(&self, init: &SystemInit) -> std::result::Result<(), String> {
        session_checks(init, self.thread())
    }
}

/// A Claude session that a matching `init` named and the turn port durably recorded: the only
/// ground for `--resume` (§3.6). A preallocated id the CLI never named cannot be resumed (WA0 E8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReportedSession(Uuid);
impl ReportedSession {
    /// Only a recorded init names one; see [`super::WorkerInitRecord::resume`].
    pub(super) fn recorded(session: Uuid) -> Self {
        Self(session)
    }
}

/// How a turn opens its Claude session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerStart {
    /// The first turn, or one whose session no recorded init named: `--session-id`, which the
    /// CLI accepts again for a session that never took input (E8).
    New(Uuid),
    /// A later turn of a session a recorded init named: `--resume`.
    Resume(ReportedSession),
}
impl WorkerStart {
    pub(crate) fn role(self) -> WorkerRole {
        self
    }

    pub(crate) fn thread(&self) -> Uuid {
        match self {
            Self::New(thread) | Self::Resume(ReportedSession(thread)) => *thread,
        }
    }
}

/// The worker's argv (§3.4): today's worker command line on the stream-json head, with the prompt
/// on stdin instead of a positional argument. No `--setting-sources` and no
/// `--disable-slash-commands`: the owner's settings, skills and plugins stay in force (OQ4).
pub(crate) struct WorkerArgs<'a> {
    pub start: WorkerStart,
    /// The card's hook settings file (`routes::claude_cards::build_claude_worker_settings_json`).
    pub settings: &'a Path,
    pub mcp_shim: &'a Path,
    pub system_prompt: &'a str,
}

impl LaunchRole for WorkerArgs<'_> {
    fn argv(&self) -> Result<Vec<OsString>> {
        let (flag, thread) = match self.start {
            WorkerStart::New(thread) => ("--session-id", thread),
            WorkerStart::Resume(ReportedSession(thread)) => ("--resume", thread),
        };
        let mut args = stream_json_args();
        args.push(flag.into());
        args.push(thread.to_string().into());
        args.extend(
            CLAUDE_WORKER_PERMISSION_FLAGS
                .split_ascii_whitespace()
                .map(OsString::from),
        );
        args.push("--settings".into());
        args.push(self.settings.as_os_str().to_owned());
        // `--strict-mcp-config` keeps `mcp__neige` scoped to the kernel's server (`worker_mcp`).
        args.push("--strict-mcp-config".into());
        args.push("--mcp-config".into());
        args.push(claude_mcp_config_json(self.mcp_shim)?.into());
        // A list option: the next token is an option, never a tool name.
        args.push("--allowedTools".into());
        args.push(format!("mcp__{MCP_SERVER_KEY}").into());
        args.push("--append-system-prompt".into());
        args.push(self.system_prompt.into());
        Ok(args)
    }
}
