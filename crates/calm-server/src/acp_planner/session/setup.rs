//! Native attachment and configuration shared by input and maintenance issuance.
use super::*;
use crate::harness::native_admission::NativePromptAdmission;

pub(super) async fn attach(
    process: &mut Process,
    params: &SessionParams,
    thread: &str,
    original_native: Option<String>,
    mcp: &[protocol::McpServer],
    admission: &NativePromptAdmission<'_>,
) -> std::result::Result<(String, provider::acp::configuration::Configuration), StartFailure> {
    let (native, configuration) = match original_native {
        Some(native) => {
            // We retain our own transcript: prefer a declared attach without replay.
            // An existing binding is never replaced, including after a native refusal.
            let (method, request) = if process.capabilities.session_capabilities.resume.is_some() {
                (
                    "session/resume",
                    protocol::encode_resume_session(&native, &params.cwd, mcp),
                )
            } else if process.capabilities.load_session {
                (
                    "session/load",
                    protocol::encode_load_session(&native, &params.cwd, mcp),
                )
            } else {
                return Err(super::start::ContinuationRefusal::LoadUnsupported.into());
            };
            let result = setup_request(
                &mut process.connection,
                &mut process.commands,
                method,
                request,
                admission.gate(),
            )
            .await?;
            (
                native,
                provider::acp::configuration::Configuration::from_response(&result)
                    .map_err(|error| StartFailure::Failed(wire_error(error)))?,
            )
        }
        None => {
            let result = setup_request(
                &mut process.connection,
                &mut process.commands,
                "session/new",
                protocol::encode_new_session(&params.cwd, mcp),
                admission.gate(),
            )
            .await?;
            let configuration = provider::acp::configuration::Configuration::from_response(&result)
                .map_err(|error| StartFailure::Failed(wire_error(error)))?;
            let native: protocol::NewSessionResponse = protocol::decode(result)
                .map_err(|error| StartFailure::Failed(wire_error(error)))?;
            if native.session_id.is_empty() {
                return Err(refused("ACP agent returned an empty native identity"));
            }
            bind_native(params, thread, &native.session_id).await?;
            (native.session_id, configuration)
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
    ) -> std::result::Result<(), StartFailure> {
        let params = &self.params;
        params
            .host
            .record_configuration(&params.card_id, configuration.clone());
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
                .ok_or_else(|| {
                    refused("The ACP agent did not declare that model or effort choice")
                })?;
            let result = setup_request(
                &mut process.connection,
                &mut process.commands,
                "session/set_config_option",
                protocol::encode_set_config_option(native, &option.id, value),
                admission.gate(),
            )
            .await?;
            configuration = provider::acp::configuration::Configuration::from_response(&result)
                .map_err(|error| StartFailure::Failed(wire_error(error)))?;
            if configuration
                .category(category)
                .is_none_or(|option| option.current_value != value)
            {
                return Err(refused("ACP did not confirm the selected model or effort"));
            }
            params
                .host
                .record_configuration(&params.card_id, configuration.clone());
        }
        Ok(())
    }
}

impl SessionParams {
    pub(super) fn mcp_server(&self, name: String, token: &str) -> protocol::McpServer {
        protocol::McpServer::Stdio {
            name,
            command: self.host.mcp_shim.to_string_lossy().into_owned(),
            // Native MCP consumers may build model output from content alone.
            args: vec!["--structured-content-as-text".into()],
            env: crate::mcp_server::wiring::card_mcp_env(&self.host.mcp_socket, token)
                .into_iter()
                .map(|(name, value)| protocol::EnvVariable {
                    name: name.into(),
                    value,
                })
                .collect(),
        }
    }
}
