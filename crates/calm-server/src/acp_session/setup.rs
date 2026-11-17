//! Native attachment and model configuration. Callers supply admission for every real write.
use crate::{
    acp_session::{
        failure::SetupFailure as StartFailure,
        process::{Commands, setup_request},
    },
    error::CalmError,
};
use provider::{
    acp::{configuration::Configuration, protocol, usage::UsageTracker},
    send_boundary::PreSendGate,
};
use serde_json::Value;
fn wire_error(error: provider::acp::Error) -> CalmError {
    CalmError::Conflict(error.to_string())
}
fn refused(message: &str) -> StartFailure {
    StartFailure::Refused(message.into())
}
pub(crate) fn attachment_request(
    capabilities: &protocol::AgentCapabilities,
    cwd: &std::path::Path,
    native: Option<&str>,
    mcp: &[protocol::McpServer],
) -> std::result::Result<(&'static str, Value), StartFailure> {
    Ok(match native {
        Some(native) if capabilities.session_capabilities.resume.is_some() => (
            "session/resume",
            protocol::encode_resume_session(native, cwd, mcp),
        ),
        Some(native) if capabilities.load_session => (
            "session/load",
            protocol::encode_load_session(native, cwd, mcp),
        ),
        Some(_) => return Err(super::failure::SetupFailure::LoadUnsupported),
        None => ("session/new", protocol::encode_new_session(cwd, mcp)),
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn configure(
    connection: &mut provider::acp::Connection,
    commands: &Commands,
    native: &str,
    configuration: &mut Configuration,
    selection: &provider::TurnModelSelection,
    gate: &PreSendGate<'_>,
    usage: &mut UsageTracker,
    mut observe: impl FnMut(&Configuration),
) -> Result<(), StartFailure> {
    observe(configuration);
    for (category, value) in [
        ("model", selection.model.as_deref()),
        ("thought_level", selection.effort.as_deref()),
    ] {
        let Some(value) = value else {
            continue;
        };
        let option = configuration
            .category(category)
            .filter(|option| option.accepts(value))
            .ok_or_else(|| refused("The ACP agent did not declare that model or effort choice"))?;
        let result = setup_request(
            connection,
            commands,
            "session/set_config_option",
            protocol::encode_set_config_option(native, &option.id, value),
            gate,
        )
        .await?;
        usage.configure(&result);
        *configuration = Configuration::from_response(&result)
            .map_err(|error| StartFailure::Failed(wire_error(error)))?;
        if configuration
            .category(category)
            .is_none_or(|option| option.current_value != value)
        {
            return Err(refused("ACP did not confirm the selected model or effort"));
        }
        observe(configuration);
    }
    Ok(())
}

pub(crate) fn mcp_server(
    host: &crate::acp_session::host::AcpHost,
    provider: &calm_types::runtime::AgentProvider,
    name: String,
    token: &str,
) -> protocol::McpServer {
    let profile = crate::acp_session::profile::AcpAgentProfile::of(provider).expect("ACP profile");
    protocol::McpServer::Stdio {
        args: profile.mcp_shim_args(&name),
        name,
        command: host.mcp_shim.to_string_lossy().into_owned(),
        env: crate::mcp_server::wiring::card_mcp_env(&host.mcp_socket, token)
            .into_iter()
            .map(|(name, value)| protocol::EnvVariable {
                name: name.into(),
                value,
            })
            .collect(),
    }
}
