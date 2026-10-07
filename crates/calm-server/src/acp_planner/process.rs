use super::config::{AcpAgentConfig, AcpPlannerHost, MARKER_KEY};
use crate::error::{CalmError, Result};
use crate::planner_permission_mode::PlannerPermissionMode;
use plugin::child_process::{GroupChild, set_process_group_leader, spawn_within};
use provider::acp::{Connection, Incoming, approvals::refuse, protocol};
use serde_json::Value;
use std::time::Duration;
use tokio::process::Command;

pub(super) struct Process {
    child: GroupChild,
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
        set_process_group_leader(&mut command);
        let mut child = spawn_within(
            command,
            tokio::time::Instant::now() + Duration::from_secs(10),
        )
        .await
        .map_err(|_| CalmError::Conflict("ACP process spawn timed out".into()))??;
        let connection = Connection::new(
            child
                .stdout()
                .ok_or_else(|| CalmError::Internal("ACP stdout missing".into()))?,
            child
                .stdin()
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
                reap(&mut child).await;
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
        reap(&mut self.child).await;
        result
    }
}

async fn reap(child: &mut GroupChild) {
    if let Ok((_, group)) =
        tokio::time::timeout(Duration::from_secs(3), child.wait_and_release_group()).await
    {
        group.sweep();
    }
    // A cancelled/timed-out wait leaves GroupChild armed; Drop sweeps before reaping.
}

pub(super) async fn setup_request(
    process: &mut Process,
    method: &str,
    params: Value,
) -> Result<Value> {
    let response = process
        .connection
        .client
        .submit(method, params)
        .await
        .map_err(|error| CalmError::Conflict(error.to_string()))?;
    let response = response.wait(Duration::from_secs(10));
    tokio::pin!(response);
    loop {
        tokio::select! {
            result=&mut response=>return finish_setup(&process.connection.client,&mut process.connection.incoming,result).await,
            incoming=process.connection.incoming.recv()=>match incoming {
                Some(Incoming::Request{id,method,..})=>refuse(&process.connection.client,id,&method).await.map_err(|error| CalmError::Conflict(error.to_string()))?,
                Some(Incoming::Notification{..})=>{}, // Loaded replay never represents a fresh native prompt.
                None=>return Err(CalmError::Conflict("ACP setup connection closed".into())),
            }
        }
    }
}
async fn finish_setup(
    client: &provider::acp::Client,
    incoming: &mut tokio::sync::mpsc::Receiver<Incoming>,
    result: std::result::Result<Value, provider::acp::Error>,
) -> Result<Value> {
    // The transport delivered these frames before the response. Drain replay at this boundary,
    // even when select chooses the ready response before the incoming branch.
    while let Ok(frame) = incoming.try_recv() {
        if let Incoming::Request { id, method, .. } = frame {
            refuse(client, id, &method)
                .await
                .map_err(|error| CalmError::Conflict(error.to_string()))?;
        }
    }
    result.map_err(|error| CalmError::Conflict(error.to_string()))
}

#[cfg(test)]
mod setup_tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    #[tokio::test]
    async fn ready_setup_response_drains_prior_history_before_live_turn_installation() {
        let (kernel, peer) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(kernel);
        let mut connection = provider::acp::Connection::new(read, write);
        let pending = connection
            .client
            .submit("session/load", json!({"sessionId":"native"}))
            .await
            .unwrap();
        let (read, mut write) = tokio::io::split(peer);
        let mut read = BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let history = json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"native","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"historic reply"}}}});
        let response = json!({"jsonrpc":"2.0","id":request["id"],"result":{}});
        write
            .write_all(format!("{history}\n{response}\n").as_bytes())
            .await
            .unwrap();
        let result = pending.wait(Duration::from_secs(2)).await;
        assert_eq!(
            connection.incoming.len(),
            1,
            "history precedes its load response on the real transport"
        );
        finish_setup(&connection.client, &mut connection.incoming, result)
            .await
            .unwrap();
        assert!(
            matches!(
                connection.incoming.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "load history must not leak into the next live translator"
        );
    }
}
