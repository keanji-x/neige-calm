//! OpenCode's ACP agent profile. Its permission policy is OpenCode's own `permission`
//! configuration, overridden through the environment, since ACP has no method for it.
use super::AcpAgentProfile;

/// What OpenCode's own `permission` configuration is overridden with under `ask` (#2348): the
/// tools that act without asking under its defaults ask instead. OpenCode 1.18.35 merges this
/// variable over every config file's top-level `permission`, and has no flag or ACP method for it.
/// `edit` covers its write and patch tools too. An agent-level `permission` in the operator profile
/// (`agent.build.permission.edit = "allow"`) still wins over it, so `external_directory` keeps its
/// default `ask`: it is what still asks before a write outside the workspace then (#2452). The
/// cost is a second ask for one outside command or edit.
const ASK_PERMISSION: (&str, &str) = (
    "OPENCODE_PERMISSION",
    r#"{"bash":"ask","edit":"ask","webfetch":"ask"}"#,
);

/// What OpenCode's `permission` configuration is overridden with under `full` (#2441): every
/// tool that acts, reads or fetches is allowed without asking, outside the workspace too. No `*`,
/// so OpenCode keeps denying the tools a Planner has no use for (`question`, `plan_*`).
const FULL_PERMISSION: (&str, &str) = (
    "OPENCODE_PERMISSION",
    r#"{"bash":"allow","edit":"allow","webfetch":"allow","external_directory":"allow","doom_loop":"allow","read":"allow"}"#,
);

pub(super) const PROFILE: AcpAgentProfile = AcpAgentProfile {
    ask_env: &[ASK_PERMISSION],
    full_env: &[FULL_PERMISSION],
};
