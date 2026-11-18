//! Native attachment and configuration shared by input and maintenance issuance.
use super::*;
use crate::harness::native_admission::NativePromptAdmission;

pub(super) fn attachment_request(
    capabilities: &protocol::AgentCapabilities,
    cwd: &std::path::Path,
    native: Option<&str>,
    mcp: &[protocol::McpServer],
) -> std::result::Result<(&'static str, Value), StartFailure> {
    crate::acp_session::setup::attachment_request(capabilities, cwd, native, mcp)
        .map_err(Into::into)
}

pub(super) async fn attach(
    process: &mut Process,
    params: &SessionParams,
    thread: &str,
    original_native: Option<String>,
    mcp: &[protocol::McpServer],
    admission: &NativePromptAdmission<'_>,
    usage: &mut provider::acp::usage::UsageTracker,
) -> std::result::Result<(String, provider::acp::configuration::Configuration), StartFailure> {
    let (method, request) = attachment_request(
        &process.capabilities,
        &params.cwd,
        original_native.as_deref(),
        mcp,
    )?;
    let result = setup_request(
        &mut process.connection,
        &process.commands,
        method,
        request,
        admission.gate(),
    )
    .await?;
    usage.configure(&result);
    let configuration = provider::acp::configuration::Configuration::from_response(&result)
        .map_err(|error| StartFailure::Failed(wire_error(error)))?;
    let native = match original_native {
        Some(native) => native,
        None => {
            let native: protocol::NewSessionResponse =
                protocol::decode(result).map_err(wire_error)?;
            if native.session_id.is_empty() {
                return Err(refused("ACP agent returned an empty native identity"));
            }
            bind_native(params, thread, &native.session_id).await?;
            native.session_id
        }
    };
    Ok((native, configuration))
}

impl Shared {
    pub(super) async fn configure(
        &self,
        process: &mut Process,
        native: &str,
        mut configuration: provider::acp::configuration::Configuration,
        selection: &provider::TurnModelSelection,
        admission: &NativePromptAdmission<'_>,
        usage: &mut provider::acp::usage::UsageTracker,
    ) -> std::result::Result<(), StartFailure> {
        self.configure_connection(
            &mut process.connection,
            &process.commands,
            native,
            &mut configuration,
            selection,
            admission,
            usage,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn configure_connection(
        &self,
        connection: &mut provider::acp::Connection,
        commands: &super::super::process::Commands,
        native: &str,
        configuration: &mut provider::acp::configuration::Configuration,
        selection: &provider::TurnModelSelection,
        admission: &NativePromptAdmission<'_>,
        usage: &mut provider::acp::usage::UsageTracker,
    ) -> std::result::Result<(), StartFailure> {
        let params = &self.params;
        crate::acp_session::setup::configure(
            connection,
            commands,
            native,
            configuration,
            selection,
            admission.gate(),
            usage,
            |configuration| {
                params
                    .host
                    .record_configuration(&params.card_id, configuration.clone())
            },
        )
        .await
        .map_err(Into::into)
    }
}

impl SessionParams {
    pub(super) fn mcp_server(&self, name: String, token: &str) -> protocol::McpServer {
        crate::acp_session::setup::mcp_server(&self.host, &self.provider, name, token)
    }
}
