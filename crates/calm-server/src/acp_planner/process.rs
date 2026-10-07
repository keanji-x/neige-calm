use super::config::{AcpAgentConfig, AcpPlannerHost, MARKER_KEY};
use crate::error::{CalmError, Result};
use crate::planner_permission_mode::PlannerPermissionMode;
use provider::acp::{Connection, protocol};
use std::time::Duration;
use tokio::process::{Child, Command};

pub(super) struct Process {
    child: Child,
    pub connection: Connection,
    pub capabilities: protocol::AgentCapabilities,
}
/// Only a registered operational process gets its active card's CLI authority.
pub(super) enum LaunchContext<'a> {
    Readiness,
    /// One Planner turn, under the permission mode the harness resolved for it.
    Planner {
        mcp_token: &'a str,
        permission: PlannerPermissionMode,
    },
}
impl Process {
    pub async fn spawn(
        host: &AcpPlannerHost,
        config: &AcpAgentConfig,
        worker: &str,
        cwd: &std::path::Path,
        context: LaunchContext<'_>,
    ) -> Result<Self> {
        crate::planner_process::stop(&host.instance, worker).await?;
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .current_dir(cwd)
            .env_clear()
            .envs(&config.env)
            .env("PATH", crate::kernel_bin_path::kernel_led_path()?.path)
            .env(MARKER_KEY, host.instance.marker(worker))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if let LaunchContext::Planner {
            mcp_token,
            permission,
        } = context
        {
            command.envs(crate::mcp_server::wiring::card_mcp_env(
                &host.mcp_socket,
                mcp_token,
            ));
            command.envs(config.permission_env(permission));
        }
        let mut child = command.spawn()?;
        let connection = Connection::new(
            child
                .stdout
                .take()
                .ok_or_else(|| CalmError::Internal("ACP stdout missing".into()))?,
            child
                .stdin
                .take()
                .ok_or_else(|| CalmError::Internal("ACP stdin missing".into()))?,
        );
        let setup =
            protocol::initialize(&connection.client, "neige", env!("CARGO_PKG_VERSION")).await;
        match setup {
            Ok(response)
                if response.agent_info.as_ref().is_some_and(|info| {
                    info.name == config.expected_agent_name
                        && info.version == config.expected_agent_version
                }) =>
            {
                Ok(Self {
                    child,
                    connection,
                    capabilities: response.agent_capabilities,
                })
            }
            _ => {
                connection.client.close();
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                crate::planner_process::stop(&host.instance, worker).await?;
                Err(CalmError::Conflict(
                    "ACP agent did not negotiate the registered protocol identity/version".into(),
                ))
            }
        }
    }
    pub async fn stop(mut self, host: &AcpPlannerHost, worker: &str) -> Result<()> {
        self.connection.client.close();
        let result = crate::planner_process::stop(&host.instance, worker).await;
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await;
        result
    }
}
