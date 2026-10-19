//! Static protocol capabilities owned by the managed backend, before a session opens.
use crate::session_projection_repo::AgentProvider;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
pub struct ManagedCapabilities {
    pub managed_conversation: bool,
    /// Whether queued input can join a running turn.
    pub steer: bool,
    pub images: bool,
    pub manual_compact: bool,
    pub history_replacement: bool,
    /// Whether a failed availability/authentication probe refuses initial creation.
    pub requires_ready_availability: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum ToolDiscovery {
    DeferredSearch,
    DirectMcp,
    SessionToolList,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogProtocol {
    Codex,
    Claude,
    Acp,
}

pub(crate) struct BackendMetadata {
    /// Whether queued input can join a running turn.
    pub steer: bool,
    pub catalog: CatalogProtocol,
    pub capabilities: ManagedCapabilities,
    pub discovery: ToolDiscovery,
    pub requires_codex_daemon: bool,
}

impl super::backend::PlannerBackend {
    /// The registrations whose protocol implementation this backend owns. This is shared
    /// with availability; consumers must not maintain a profile-specific provider list.
    pub(crate) fn registered_providers() -> impl Iterator<Item = AgentProvider> {
        [
            AgentProvider::Codex,
            AgentProvider::Claude,
            AgentProvider::OpenCode,
        ]
        .into_iter()
    }

    pub(crate) fn metadata(provider: &AgentProvider) -> BackendMetadata {
        let (steer, images, manual_compact, history_replacement, discovery) = match provider {
            AgentProvider::Codex => (true, true, true, true, ToolDiscovery::DeferredSearch),
            AgentProvider::Claude => (false, true, true, true, ToolDiscovery::DirectMcp),
            AgentProvider::OpenCode => (false, false, false, false, ToolDiscovery::SessionToolList),
        };
        let catalog = match provider {
            AgentProvider::Codex => CatalogProtocol::Codex,
            AgentProvider::Claude => CatalogProtocol::Claude,
            AgentProvider::OpenCode => CatalogProtocol::Acp,
        };
        let (requires_ready_availability, requires_codex_daemon) = match provider {
            AgentProvider::Codex => (false, true),
            AgentProvider::Claude | AgentProvider::OpenCode => (true, false),
        };
        BackendMetadata {
            steer,
            catalog,
            requires_codex_daemon,
            capabilities: ManagedCapabilities {
                managed_conversation: true,
                steer,
                images,
                manual_compact,
                history_replacement,
                requires_ready_availability,
            },
            discovery,
        }
    }
}

impl ToolDiscovery {
    pub(crate) fn render_assistant(self, template: &str) -> String {
        let tools = match self {
            Self::DeferredSearch => return template.to_owned(),
            Self::DirectMcp => {
                "## Report tools\n\nNeige report tools are already in your tool list as `mcp__neige__<name>`; call them directly.\n\n"
            }
            Self::SessionToolList => {
                "## Report tools\n\nUse the Neige MCP report tools in the tool list supplied by this session. Tool listing is not a grant; only Assistant tools are authorized.\n\n"
            }
        };
        template
            .replace("{assistant_tool_discovery}", tools)
            .replace(include_str!("../../prompts/tool-discovery.md"), "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steer_capability_projects_backend_metadata_for_each_provider() {
        for provider in super::super::backend::PlannerBackend::registered_providers() {
            let metadata = super::super::backend::PlannerBackend::metadata(&provider);
            let serialized = serde_json::to_value(metadata.capabilities).unwrap();
            assert_eq!(serialized["steer"], metadata.steer, "{provider:?}");
            assert_eq!(metadata.steer, matches!(provider, AgentProvider::Codex));
        }
    }
}
