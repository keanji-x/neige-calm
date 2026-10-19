//! Per-provider ACP agent profiles: what one agent needs beyond the ACP contract, such as where
//! its `initialize` answer reports its identity, the method it authenticates with, and the agent's
//! own configuration surface its launch sets. Within the ACP planner, a provider's policy is
//! reached only through [`AcpAgentProfile::of`], so inside this module tree a provider speaking
//! ACP adds one module here and one arm in [`AcpAgentProfile::of`]. That is the ACP planner's
//! share only: the provider itself is still declared where each owner keeps it (its variant and
//! mappings in `calm_types`, its persisted `worker_sessions.provider` value, its backend
//! capabilities and execution registration, and the frontend's provider choices).
use crate::planner_permission_mode::PlannerPermissionMode;
use calm_types::runtime::AgentProvider;
use provider::acp::protocol::InitializeResponse;

mod opencode;

pub(crate) struct AcpAgentProfile {
    /// Where the agent's `initialize` answer reports who it is.
    pub(crate) identity: AgentIdentity,
    /// The reported `authMethods` id every launch authenticates with after `initialize`, before
    /// any session operation; `None` sends no `authenticate`.
    pub(crate) authentication: Option<&'static str>,
    /// What one Planner turn's process adds under `ask`.
    pub(crate) ask: ModeLaunch,
    /// What one Planner turn's process adds under `full`.
    pub(crate) full: ModeLaunch,
}

/// Where one agent's `initialize` answer reports its name and version: a closed set of the forms
/// registered agents use.
pub(crate) enum AgentIdentity {
    /// ACP's `agentInfo`, whose `name` must be this.
    AgentInfo { name: &'static str },
    /// No `agentInfo`, only a version: the string at `_meta.agentVersion` (Grok Build 1.0.62).
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Grok Build's form; its provider registers it")
    )]
    MetaAgentVersion,
}

/// What one Planner turn's launch adds under one permission mode, set explicitly, never inherited.
pub(crate) struct ModeLaunch {
    pub(crate) env: &'static [(&'static str, &'static str)],
    /// Agent options, inserted between the registration's `args` and its `subcommand`.
    pub(crate) agent_options: &'static [&'static str],
}

/// `never` adds nothing: the agent's own configuration stands.
const NOTHING: ModeLaunch = ModeLaunch {
    env: &[],
    agent_options: &[],
};

impl AcpAgentProfile {
    /// The profile of `provider`; `None` for one that does not speak ACP.
    pub(crate) fn of(provider: &AgentProvider) -> Option<&'static Self> {
        match provider {
            AgentProvider::OpenCode => Some(&opencode::PROFILE),
            AgentProvider::Codex | AgentProvider::Claude => None,
        }
    }

    /// What one Planner turn's process adds under `mode`.
    pub(crate) fn launch(&self, mode: PlannerPermissionMode) -> &ModeLaunch {
        match mode {
            PlannerPermissionMode::Never => &NOTHING,
            PlannerPermissionMode::Ask => &self.ask,
            PlannerPermissionMode::Full => &self.full,
        }
    }
}

impl AgentIdentity {
    /// Whether `response` reports this identity at a version of at least `minimum`. A version
    /// that is not semver is never accepted.
    pub(crate) fn admits(&self, response: &InitializeResponse, minimum: &semver::Version) -> bool {
        let reported = match self {
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
        };
        reported
            .and_then(|version| semver::Version::parse(version).ok())
            .is_some_and(|version| version >= *minimum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::runtime::AgentProtocol;
    use serde_json::json;

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
        let minimum = semver::Version::parse("1.18.35").unwrap();
        for (version, admitted) in [
            ("1.18.35", true),
            ("1.18.36", true),
            ("2.0.0", true),
            ("1.18.34", false),
            ("1.18.35-beta.1", false),
            ("1.18", false),
            ("v1.18.35", false),
            ("wrong", false),
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
