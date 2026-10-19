//! Per-provider ACP agent profiles: what one agent needs beyond the ACP contract, such as the
//! agent's own configuration surface its launch sets. Within the ACP planner, a provider's policy
//! is reached only through [`AcpAgentProfile::of`], so inside this module tree a provider speaking
//! ACP adds one module here and one arm in [`AcpAgentProfile::of`]. That is the ACP planner's
//! share only: the provider itself is still declared where each owner keeps it (its variant and
//! mappings in `calm_types`, its persisted `worker_sessions.provider` value, its backend
//! capabilities and execution registration, and the frontend's provider choices).
use crate::planner_permission_mode::PlannerPermissionMode;
use calm_types::runtime::AgentProvider;

mod opencode;

pub(crate) struct AcpAgentProfile {
    /// What one Planner turn's process adds under `ask`.
    ask_env: &'static [(&'static str, &'static str)],
    /// What one Planner turn's process adds under `full`.
    full_env: &'static [(&'static str, &'static str)],
}

impl AcpAgentProfile {
    /// The profile of `provider`; `None` for one that does not speak ACP.
    pub(crate) fn of(provider: &AgentProvider) -> Option<&'static Self> {
        match provider {
            AgentProvider::OpenCode => Some(&opencode::PROFILE),
            AgentProvider::Codex | AgentProvider::Claude => None,
        }
    }

    /// The environment one Planner turn's process adds under `mode`. `never` adds nothing: the
    /// agent's own configuration stands.
    pub(crate) fn permission_env(
        &self,
        mode: PlannerPermissionMode,
    ) -> Vec<(&'static str, &'static str)> {
        match mode {
            PlannerPermissionMode::Never => Vec::new(),
            PlannerPermissionMode::Ask => self.ask_env.to_vec(),
            PlannerPermissionMode::Full => self.full_env.to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::runtime::AgentProtocol;

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
}
