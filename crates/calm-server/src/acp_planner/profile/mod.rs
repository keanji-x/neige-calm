//! Per-provider ACP agent profiles: what one agent needs beyond the ACP contract, such as the
//! argv its CLI serves ACP under, where its `initialize` answer reports its identity, the method
//! it authenticates with, and the agent's own configuration surface its launch sets. Within the ACP planner, a provider's policy is
//! reached only through [`AcpAgentProfile::of`], so inside this module tree a provider speaking
//! ACP adds one module here and one arm in [`AcpAgentProfile::of`]. That is the ACP planner's
//! share only: the provider itself is still declared where each owner keeps it (its variant and
//! mappings in `calm_types`, its persisted `worker_sessions.provider` value, its backend
//! capabilities and execution registration, and the frontend's provider choices).
use crate::planner_permission_mode::PlannerPermissionMode;
use calm_types::runtime::AgentProvider;
use provider::acp::approvals::{AgentAsks, McpToolRequest, OwnTools};
use provider::acp::protocol::InitializeResponse;

mod grok;
mod opencode;

pub(crate) struct AcpAgentProfile {
    /// Supports receipt-owned canonical Planner input; activation is independently gated.
    pub(crate) canonical_planner_inputs: bool,
    /// Supports receipt-owned canonical Assistant input, independently of Planner policy.
    pub(crate) canonical_assistant_inputs: bool,
    /// The agent CLI's ACP entry: the arguments around a Planner turn's agent options.
    pub(crate) entry: AgentEntry,
    /// Where the agent's `initialize` answer reports who it is.
    pub(crate) identity: AgentIdentity,
    /// Supported native contract, independently of an operator's additional minimum.
    pub(crate) versions: Option<AgentVersions>,
    /// Environment content owned by the profile, never overwritten in a registration.
    pub(crate) owned_env: &'static [&'static str],
    /// What a Planner turn adds under `never`.
    pub(crate) never: ModeLaunch,
    /// The reported `authMethods` id every launch authenticates with after `initialize`, before
    /// any session operation; `None` sends no `authenticate`.
    pub(crate) authentication: Option<&'static str>,
    /// What one Planner turn's process adds under `ask`.
    pub(crate) ask: ModeLaunch,
    /// What one Planner turn's process adds under `full`.
    pub(crate) full: ModeLaunch,
    /// How the agent asks before calling an MCP tool, when it does: such a request for a tool
    /// Neige's own server lists the session is approved under every mode, since that server
    /// authorizes each call by the card's token. Each launch then registers that server under a
    /// key of its own ([`Self::mcp_server_key`]), so no other server can take its name. `None`
    /// answers it as the mode answers any request, and keeps the shared key.
    pub(crate) mcp_tool_request: Option<McpToolRequest>,
    /// The agent ends its turn `cancelled` when a request it asked is declined, so the turn ends
    /// with the decline as its reason rather than as interrupted.
    pub(crate) decline_ends_turn: bool,
    /// Environment the registration may not set: credentials the agent would use in place of the
    /// sign-in it authenticates with.
    pub(crate) refused_env: &'static [&'static str],
}

/// A profile's supported release line and tested floor. Other majors, including
/// their prereleases, are refused even if semver precedence is below the next release.
pub(crate) struct AgentVersions {
    pub(crate) major: u64,
    pub(crate) floor: (u64, u64, u64),
    pub(crate) refusal: &'static str,
}

/// The argv an agent CLI serves ACP under, after the registration's `args` (which only prefix
/// it, such as a wrapper's or interpreter's own arguments): `before_options`, then the agent
/// options of the turn's permission mode, then `after_options`. Where the options go is the
/// CLI's own parsing rule, so its profile states it rather than the operator.
pub(crate) struct AgentEntry {
    pub(crate) before_options: &'static [&'static str],
    pub(crate) after_options: &'static [&'static str],
}

/// Where one agent's `initialize` answer reports its name and version: a closed set of the forms
/// registered agents use.
pub(crate) enum AgentIdentity {
    /// ACP's `agentInfo`, whose `name` must be this.
    AgentInfo { name: &'static str },
    /// No `agentInfo`, only a version: the string at `_meta.agentVersion` (Grok Build 1.0.62).
    MetaAgentVersion,
}

/// What one Planner turn's launch adds under one permission mode, set explicitly, never inherited.
pub(crate) struct ModeLaunch {
    pub(crate) env: &'static [(&'static str, &'static str)],
    /// Agent options, inserted inside the profile's [`AgentEntry`].
    pub(crate) agent_options: &'static [&'static str],
}

/// A launch that adds nothing: the agent's own configuration stands.
const NOTHING: ModeLaunch = ModeLaunch {
    env: &[],
    agent_options: &[],
};

impl AcpAgentProfile {
    pub(crate) fn admit_identity(
        &self,
        response: &InitializeResponse,
        minimum: &semver::Version,
    ) -> Result<(), String> {
        if let Some(versions) = &self.versions {
            let (major, minor, patch) = versions.floor;
            let floor = semver::Version::new(major, minor, patch);
            if !self.identity.admits(response, &floor)
                || self
                    .identity
                    .reported(response)
                    .and_then(|value| semver::Version::parse(value).ok())
                    .is_none_or(|value| value.major != versions.major)
            {
                return Err(versions.refusal.to_owned());
            }
        }
        if !self.identity.admits(response, minimum) {
            return Err(format!(
                "ACP agent did not negotiate the registered protocol identity at version {minimum} or later"
            ));
        }
        Ok(())
    }

    pub(crate) fn canonical_inputs(&self, contract: calm_types::worker::WorkerContract) -> bool {
        use calm_types::worker::WorkerContract;
        match contract {
            WorkerContract::Planner => self.canonical_planner_inputs,
            WorkerContract::Assistant => self.canonical_assistant_inputs,
            WorkerContract::Executor | WorkerContract::Validator => false,
        }
    }

    /// The profile of `provider`; `None` for one that does not speak ACP.
    pub(crate) fn of(provider: &AgentProvider) -> Option<&'static Self> {
        match provider {
            AgentProvider::OpenCode => Some(&opencode::PROFILE),
            AgentProvider::Grok => Some(&grok::PROFILE),
            AgentProvider::Codex | AgentProvider::Claude => None,
        }
    }

    /// What one Planner turn's process adds under `mode`.
    pub(crate) fn launch(&self, mode: PlannerPermissionMode) -> &ModeLaunch {
        match mode {
            PlannerPermissionMode::Never => &self.never,
            PlannerPermissionMode::Ask => &self.ask,
            PlannerPermissionMode::Full => &self.full,
        }
    }

    /// The key one launch registers Neige's MCP server under: a fresh one for an agent whose
    /// requests for MCP tools are approved by name, never holding `_`, so no server the agent
    /// loads from elsewhere can know or share it; otherwise the shared key.
    pub(crate) fn mcp_server_key(&self) -> String {
        let key = crate::mcp_server::wiring::MCP_SERVER_KEY;
        match self.mcp_tool_request {
            Some(_) => format!("{key}-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
            None => key.to_owned(),
        }
    }

    /// How this agent asks, for one turn whose launch registered Neige's server as `server`, which
    /// lists the session `tools`.
    pub(crate) fn asks<T: AsRef<str>>(
        &self,
        server: &str,
        tools: impl IntoIterator<Item = T>,
    ) -> AgentAsks {
        AgentAsks {
            own_tools: self
                .mcp_tool_request
                .and_then(|form| OwnTools::new(form, server, tools)),
            decline_ends_turn: self.decline_ends_turn,
        }
    }

    /// The argv after the registration's `args`: the entry with `launch`'s agent options inside;
    /// a launch outside a Planner turn passes `None` and adds none.
    pub(crate) fn argv<'a>(
        &'a self,
        launch: Option<&'a ModeLaunch>,
    ) -> impl Iterator<Item = &'static str> + 'a {
        let options = launch.map_or(&[][..], |launch| launch.agent_options);
        self.entry
            .before_options
            .iter()
            .chain(options)
            .chain(self.entry.after_options)
            .copied()
    }
}

impl AgentIdentity {
    /// Whether `response` reports this identity at a version of at least `minimum` by semver
    /// precedence: build metadata is ignored, and a pre-release ranks below its release, so with
    /// minimum 2.1.3 `2.1.4-beta.1` is admitted and `2.1.3-beta.1` is not. A version that
    /// is not semver is never admitted.
    pub(crate) fn admits(&self, response: &InitializeResponse, minimum: &semver::Version) -> bool {
        self.reported(response)
            .and_then(|version| semver::Version::parse(version).ok())
            .is_some_and(|version| version.cmp_precedence(minimum).is_ge())
    }

    fn reported<'a>(&self, response: &'a InitializeResponse) -> Option<&'a str> {
        match self {
            Self::AgentInfo { name } => response
                .agent_info
                .as_ref()
                .filter(|info| info.name == *name)
                .map(|info| info.version.as_str()),
            Self::MetaAgentVersion => response
                .meta
                .as_ref()
                .and_then(|meta| meta.get("agentVersion"))
                .and_then(serde_json::Value::as_str),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::runtime::AgentProtocol;
    use serde_json::json;

    #[test]
    fn acp_assistant_capability_is_independent_of_planner_contract() {
        use calm_types::worker::WorkerContract;
        for planner in [false, true] {
            for assistant in [false, true] {
                let profile = AcpAgentProfile {
                    canonical_planner_inputs: planner,
                    canonical_assistant_inputs: assistant,
                    ..opencode::PROFILE
                };
                assert_eq!(profile.canonical_inputs(WorkerContract::Planner), planner);
                assert_eq!(
                    profile.canonical_inputs(WorkerContract::Assistant),
                    assistant
                );
                assert!(!profile.canonical_inputs(WorkerContract::Executor));
                assert!(!profile.canonical_inputs(WorkerContract::Validator));
            }
        }
    }

    #[test]
    fn a_profile_exists_exactly_for_each_acp_provider() {
        for provider in AgentProvider::ALL {
            assert_eq!(
                AcpAgentProfile::of(&provider).is_some(),
                provider.protocol() == AgentProtocol::Acp,
                "{provider:?}"
            );
        }
    }

    fn initialize(fields: serde_json::Value) -> InitializeResponse {
        let mut response = json!({"protocolVersion":1,"agentCapabilities":{}});
        response
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        serde_json::from_value(response).unwrap()
    }

    #[test]
    fn a_reported_version_is_admitted_from_the_minimum_on() {
        let identity = AgentIdentity::AgentInfo { name: "OpenCode" };
        let minimum = semver::Version::parse("2.1.3").unwrap();
        for (version, admitted) in [
            ("2.1.3", true),
            ("2.1.4", true),
            ("3.0.0", true),
            ("2.1.2", false),
            // Precedence ranks a pre-release below its own release only.
            ("2.1.4-beta.1", true),
            ("2.1.3-beta.1", false),
            // Build metadata never ranks.
            ("2.1.3+vendor.2", true),
            ("2.1.2+vendor.2", false),
            ("2.1", false),
            ("v2.1.3", false),
            ("wrong", false),
        ] {
            let response = initialize(json!({"agentInfo":{"name":"OpenCode","version":version}}));
            assert_eq!(identity.admits(&response, &minimum), admitted, "{version}");
        }
    }

    /// A minimum carrying build metadata ranks as its version alone, against a reported version
    /// with no, the same or other build metadata.
    #[test]
    fn a_minimums_build_metadata_never_ranks() {
        let identity = AgentIdentity::AgentInfo { name: "OpenCode" };
        let minimum = semver::Version::parse("2.1.3+vendor.2").unwrap();
        for (version, admitted) in [
            ("2.1.3", true),
            ("2.1.3+vendor.1", true),
            ("2.1.3+vendor.2", true),
            ("2.1.2+vendor.3", false),
        ] {
            let response = initialize(json!({"agentInfo":{"name":"OpenCode","version":version}}));
            assert_eq!(identity.admits(&response, &minimum), admitted, "{version}");
        }
    }

    #[test]
    fn agent_info_must_carry_the_profiles_name() {
        let identity = AgentIdentity::AgentInfo { name: "OpenCode" };
        let minimum = semver::Version::parse("1.0.0").unwrap();
        for fields in [
            json!({"agentInfo":{"name":"Other","version":"1.0.0"}}),
            json!({}),
            json!({"_meta":{"agentVersion":"1.0.0"}}),
        ] {
            assert!(
                !identity.admits(&initialize(fields.clone()), &minimum),
                "{fields}"
            );
        }
    }

    #[test]
    fn a_meta_version_is_read_only_from_meta() {
        let identity = AgentIdentity::MetaAgentVersion;
        let minimum = semver::Version::parse("1.0.62").unwrap();
        assert!(identity.admits(
            &initialize(json!({"_meta":{"agentVersion":"1.0.62"}})),
            &minimum
        ));
        for fields in [
            json!({"_meta":{"agentVersion":"1.0.61"}}),
            json!({"_meta":{"agentVersion":62}}),
            json!({"_meta":{}}),
            json!({"agentInfo":{"name":"Grok","version":"1.0.62"}}),
        ] {
            assert!(
                !identity.admits(&initialize(fields.clone()), &minimum),
                "{fields}"
            );
        }
    }
}
