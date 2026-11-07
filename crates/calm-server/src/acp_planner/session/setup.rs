//! Native attachment and configuration shared by input and maintenance issuance.
use super::*;
use crate::harness::native_admission::NativePromptAdmission;

pub(super) fn attachment_request(
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
        Some(_) => return Err(super::start::ContinuationRefusal::LoadUnsupported.into()),
        None => ("session/new", protocol::encode_new_session(cwd, mcp)),
    })
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
    if let Some(native) = original_native.as_deref()
        && super::super::profile::AcpAgentProfile::of(&params.provider)
            .is_some_and(|profile| profile.global_native_identity)
    {
        let pool = params
            .repo
            .sqlite_db()
            .ok_or(CalmError::ExecutionUnavailable)?;
        ensure_native_owner(
            pool.read_pool(),
            &params.provider,
            &params.worker_session_id,
            native,
        )
        .await?;
    }
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
        params
            .host
            .record_configuration(&params.card_id, configuration.clone());
        let native_mode = super::super::profile::AcpAgentProfile::of(&params.provider)
            .and_then(|profile| profile.native_mode);
        for (category, value) in [
            ("mode", native_mode),
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
                    refused("The ACP agent did not declare that session configuration choice")
                })?;
            let result = setup_request(
                connection,
                commands,
                "session/set_config_option",
                protocol::encode_set_config_option(native, &option.id, value),
                admission.gate(),
            )
            .await?;
            usage.configure(&result);
            *configuration = provider::acp::configuration::Configuration::from_response(&result)
                .map_err(|error| StartFailure::Failed(wire_error(error)))?;
            if configuration
                .category(category)
                .is_none_or(|option| option.current_value != value)
            {
                return Err(refused(
                    "ACP did not confirm the selected session configuration",
                ));
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

pub(in crate::acp_planner) async fn ensure_native_owner<'e>(
    executor: impl sqlx::SqliteExecutor<'e>,
    provider: &AgentProvider,
    worker: &str,
    native: &str,
) -> Result<()> {
    let collision: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_sessions WHERE provider=?1 AND agent_session_id=?2 AND id<>?3)",
    )
    .bind(provider.wire_name())
    .bind(native)
    .bind(worker)
    .fetch_one(executor)
    .await?;
    if collision {
        return Err(CalmError::Conflict(
            "ACP native session belongs to another worker".into(),
        ));
    }
    Ok(())
}
