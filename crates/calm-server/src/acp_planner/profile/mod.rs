//! Per-provider ACP agent profiles: what one agent needs beyond the ACP contract, such as the
//! agent's own configuration surface its launch sets. The generic ACP planner reaches a provider's
//! policy only through [`AcpAgentProfile::of`] and never names a provider itself, so a provider
//! speaking ACP adds one module here and one arm in [`AcpAgentProfile::of`].
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
