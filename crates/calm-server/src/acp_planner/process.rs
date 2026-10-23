use super::config::{AcpPlannerHost, MARKER_KEY, Registration};
use crate::error::{CalmError, Result};
use crate::planner_permission_mode::PlannerPermissionMode;
use plugin::child_process::{GroupChild, set_process_group_leader, spawn_within};
use provider::acp::{Connection, Incoming, approvals::refuse, protocol};
use serde_json::Value;
use std::time::Duration;
use tokio::process::Command;

use super::start::StartFailure;

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
        registration: Registration<'_>,
        worker: &str,
        cwd: &std::path::Path,
        context: LaunchContext<'_>,
    ) -> Result<Self> {
        Self::spawn_control(host, registration, worker, cwd, context)
            .await
            .map_err(Into::into)
    }

    /// Spawn the registered agent, `initialize` it, admit its reported identity, and
    /// authenticate it when its profile names a method. Any refusal stops the process before a
    /// session operation.
    pub async fn spawn_control(
        host: &AcpPlannerHost,
        registration: Registration<'_>,
        worker: &str,
        cwd: &std::path::Path,
        context: LaunchContext<'_>,
    ) -> std::result::Result<Self, StartFailure> {
        let Registration { config, profile } = registration;
        crate::planner_process::stop(&host.instance, worker).await?;
        let launch = match context {
            LaunchContext::Planner { permission, .. } => Some(profile.launch(permission)),
            LaunchContext::Readiness => None,
        };
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .args(profile.argv(launch))
            .current_dir(cwd)
            .env_clear()
            .envs(&config.env)
            .env(
                "PATH",
                crate::kernel_bin_path::kernel_led_path()
                    .map_err(CalmError::from)?
                    .path,
            )
            .env(MARKER_KEY, host.instance.marker(worker))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let LaunchContext::Planner { mcp_token, .. } = context {
            command.envs(crate::mcp_server::wiring::card_mcp_env(
                &host.mcp_socket,
                mcp_token,
            ));
        }
        if let Some(launch) = launch {
            command.envs(launch.env.iter().copied());
        }
        set_process_group_leader(&mut command);
        let mut child = spawn_within(
            command,
            tokio::time::Instant::now() + Duration::from_secs(10),
        )
        .await
        .map_err(|_| {
            StartFailure::Failed(CalmError::Conflict("ACP process spawn timed out".into()))
        })?
        .map_err(|error| StartFailure::Failed(error.into()))?;
        keep_stderr(child.stderr(), stderr_log(host, worker));
        let connection = Connection::new(
            child
                .stdout()
                .ok_or_else(|| CalmError::Internal("ACP stdout missing".into()))?,
            child
                .stdin()
                .ok_or_else(|| CalmError::Internal("ACP stdin missing".into()))?,
        );
        match negotiate(&connection.client, registration).await {
            Ok(capabilities) => Ok(Self {
                child,
                connection,
                capabilities,
            }),
            Err(refusal) => {
                connection.client.close();
                let _ = child.start_kill();
                reap(&mut child).await;
                crate::planner_process::stop(&host.instance, worker).await?;
                Err(StartFailure::Failed(CalmError::Conflict(refusal)))
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

/// The most of one agent process's stderr that is kept; the rest is read and dropped, so the agent
/// never blocks writing it.
const STDERR_LIMIT: usize = 256 * 1024;

/// The file the stderr of `worker`'s agent processes is kept in, under the host's data directory;
/// each launch overwrites it.
fn stderr_log(host: &AcpPlannerHost, worker: &str) -> std::path::PathBuf {
    let name: String = worker
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    host.agent_logs.join(format!("{name}.stderr.log"))
}

/// Copy the first [`STDERR_LIMIT`] bytes of an agent's stderr to `path` (owner-only), and drain the
/// rest, until the process closes it. The agent's own diagnostics only: Neige writes nothing there,
/// and the card credential the process holds is revoked when its turn ends.
fn keep_stderr(stderr: Option<tokio::process::ChildStderr>, path: std::path::PathBuf) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(mut stderr) = stderr else {
        return;
    };
    tokio::spawn(async move {
        let mut file = match open_log(&path).await {
            Ok(file) => Some(file),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "ACP agent stderr is not kept");
                None
            }
        };
        let mut kept = 0;
        let mut buffer = vec![0; 8192];
        while let Ok(read @ 1..) = stderr.read(&mut buffer).await {
            let take = read.min(STDERR_LIMIT - kept);
            if take > 0
                && let Some(log) = file.as_mut()
            {
                if log.write_all(&buffer[..take]).await.is_err() {
                    file = None;
                }
                kept += take;
            }
        }
    });
}

async fn open_log(path: &std::path::Path) -> std::io::Result<tokio::fs::File> {
    if let Some(directory) = path.parent() {
        tokio::fs::create_dir_all(directory).await?;
    }
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path).await
}

/// `initialize`, then the profile's identity at the registered minimum version, then its
/// authentication: the agent's capabilities, or why the launch is refused.
async fn negotiate(
    client: &provider::acp::Client,
    Registration { config, profile }: Registration<'_>,
) -> std::result::Result<protocol::AgentCapabilities, String> {
    let response = protocol::initialize(client, "neige", env!("CARGO_PKG_VERSION"))
        .await
        .map_err(|error| format!("ACP initialize failed: {error}"))?;
    profile.admit_identity(&response, &config.min_agent_version)?;
    if let Some(method) = profile.authentication {
        // Only the agent's own report: never its private credential files.
        let signed_out = |cause: String| {
            format!(
                "{} did not authenticate ({cause}); sign it in with its own CLI under the environment \
                 its ACP registration declares, then retry",
                config.provider.wire_name()
            )
        };
        if !response
            .auth_methods
            .iter()
            .any(|offered| offered.id == method)
        {
            return Err(signed_out(format!(
                "it does not offer authentication method `{method}`"
            )));
        }
        protocol::authenticate(client, method)
            .await
            .map_err(|error| signed_out(format!("authenticate `{method}`: {error}")))?;
    }
    Ok(response.agent_capabilities)
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
    gate: &provider::send_boundary::PreSendGate<'_>,
) -> std::result::Result<Value, StartFailure> {
    let response = process
        .connection
        .client
        .submit_gated(method, params, gate)
        .await
        .map_err(|error| StartFailure::Failed(CalmError::Conflict(error.to_string())))?;
    let response = response.wait(Duration::from_secs(10));
    tokio::pin!(response);
    loop {
        tokio::select! {
            result=&mut response=>return finish_setup(&process.connection.client,&mut process.connection.incoming,result).await,
            incoming=process.connection.incoming.recv()=>match incoming {
                Some(Incoming::Request{id,method,..})=>refuse(&process.connection.client,id,&method).await.map_err(|error| StartFailure::Failed(CalmError::Conflict(error.to_string())))?,
                Some(Incoming::Notification{..})=>{}, // Loaded replay never represents a fresh native prompt.
                None=>return Err(StartFailure::Failed(CalmError::Conflict("ACP setup connection closed".into()))),
            }
        }
    }
}
async fn finish_setup(
    client: &provider::acp::Client,
    incoming: &mut tokio::sync::mpsc::Receiver<Incoming>,
    result: std::result::Result<Value, provider::acp::Error>,
) -> std::result::Result<Value, StartFailure> {
    // The transport delivered these frames before the response. Drain replay at this boundary,
    // even when select chooses the ready response before the incoming branch.
    while let Ok(frame) = incoming.try_recv() {
        if let Incoming::Request { id, method, .. } = frame {
            refuse(client, id, &method)
                .await
                .map_err(|error| StartFailure::Failed(CalmError::Conflict(error.to_string())))?;
        }
    }
    result.map_err(|error| StartFailure::Failed(CalmError::Conflict(error.to_string())))
}

#[cfg(test)]
mod setup_tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    #[tokio::test]
    async fn m1_mapping_setup_native_errors_keep_ordinary_retry_policy() {
        use provider::acp::Error;
        for error in [
            Error::Closed,
            Error::Timeout,
            Error::Protocol("invalid setup reply"),
            Error::Remote {
                code: -32602,
                message: "native refusal".into(),
                data: None,
            },
        ] {
            let diagnostic = error.to_string();
            let (kernel, _peer) = tokio::io::duplex(4096);
            let (read, write) = tokio::io::split(kernel);
            let mut connection = provider::acp::Connection::new(read, write);
            let result =
                finish_setup(&connection.client, &mut connection.incoming, Err(error)).await;
            let outcome = super::super::start::start_result(result.map(|_| unreachable!()));
            assert!(matches!(
                outcome,
                Ok(provider::control::TurnStartOutcome::Failed { error: CalmError::Conflict(message) })
                    if message == diagnostic
            ));
        }
    }

    #[tokio::test]
    async fn ready_setup_response_drains_prior_history_before_live_turn_installation() {
        let (kernel, peer) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(kernel);
        let mut connection = provider::acp::Connection::new(read, write);
        let pending = connection
            .client
            .submit_gated(
                "session/load",
                protocol::encode_load_session("native", std::path::Path::new("/workspace"), &[]),
                &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
            )
            .await
            .map_err(provider::acp::WriteFailure::into_error)
            .unwrap();
        let (read, mut write) = tokio::io::split(peer);
        let mut read = BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let history = json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"native","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"historic reply"}}}});
        let commands = json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"native","update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"compact","description":"Compact the session"}]}}});
        let config = json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"native","update":{"sessionUpdate":"config_option_update","configOptions":[]}}});
        let response = json!({"jsonrpc":"2.0","id":request["id"],"result":{}});
        write
            .write_all(format!("{history}\n{commands}\n{config}\n{response}\n").as_bytes())
            .await
            .unwrap();
        let result = pending.wait(Duration::from_secs(2)).await;
        assert_eq!(
            connection.incoming.len(),
            3,
            "history, commands and config precede the attach response on the real transport"
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

#[cfg(test)]
mod pump_tests {
    use super::*;
    use serde_json::json;

    // Real child pipes and the production setup pump; the peer controls frame ordering.
    async fn peer(script: &str) -> Process {
        let mut command = Command::new("python3");
        command
            .args(["-u", "-c", script])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        set_process_group_leader(&mut command);
        let mut child = spawn_within(
            command,
            tokio::time::Instant::now() + Duration::from_secs(2),
        )
        .await
        .unwrap()
        .unwrap();
        let connection = Connection::new(child.stdout().unwrap(), child.stdin().unwrap());
        Process {
            child,
            connection,
            capabilities: protocol::AgentCapabilities::default(),
        }
    }

    async fn cleanup(process: &mut Process) {
        process.connection.client.close();
        let _ = process.child.start_kill();
        reap(&mut process.child).await;
    }

    #[tokio::test]
    async fn setup_pump_refuses_early_peer_request_with_provider_encoder() {
        let mut process = peer(r#"
import json,sys
request=json.loads(sys.stdin.readline())
print(json.dumps({'jsonrpc':'2.0','id':'early','method':'session/request_permission','params':{}}),flush=True)
refusal=json.loads(sys.stdin.readline())
assert refusal == {'jsonrpc':'2.0','id':'early','result':{'outcome':{'outcome':'cancelled'}}}, refusal
print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':{'sessionId':'native'}}),flush=True)
sys.stdin.read()
"#).await;
        let result = setup_request(
            &mut process,
            "session/new",
            protocol::encode_new_session(std::path::Path::new("/workspace"), &[]),
            &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
        )
        .await;
        cleanup(&mut process).await;
        assert_eq!(result.unwrap(), json!({"sessionId":"native"}));
    }

    #[tokio::test]
    async fn setup_pump_drains_history_with_provider_encoder() {
        let mut process = peer(r#"
import json,sys
request=json.loads(sys.stdin.readline())
print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'native','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'history'}}}}))
print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':{}}),flush=True)
sys.stdin.read()
"#).await;
        let result = setup_request(
            &mut process,
            "session/load",
            protocol::encode_load_session("native", std::path::Path::new("/workspace"), &[]),
            &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
        )
        .await;
        assert_eq!(result.unwrap(), json!({}));
        assert!(matches!(
            process.connection.incoming.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        cleanup(&mut process).await;
    }

    #[tokio::test]
    async fn setup_pump_malformed_result_reaches_shared_decoder() {
        let mut process = peer(
            r#"
import json,sys
request=json.loads(sys.stdin.readline())
print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':{'sessionId':42}}),flush=True)
sys.stdin.read()
"#,
        )
        .await;
        let result = setup_request(
            &mut process,
            "session/new",
            protocol::encode_new_session(std::path::Path::new("/workspace"), &[]),
            &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
        )
        .await;
        cleanup(&mut process).await;
        // The pump returns the native value; shared decoding rejects it without validation
        // moving into the encoder or transport. The existing mapping test pins retry policy.
        assert!(matches!(
            protocol::decode::<protocol::NewSessionResponse>(result.unwrap()),
            Err(provider::acp::Error::Protocol("malformed method result"))
        ));
    }

    #[tokio::test]
    async fn setup_pump_disconnect_keeps_native_failure_classification() {
        let mut process = peer("import sys\nsys.stdin.readline()\n").await;
        let result = setup_request(
            &mut process,
            "session/set_config_option",
            protocol::encode_set_config_option("native", "model", "chosen"),
            &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
        )
        .await;
        cleanup(&mut process).await;
        assert!(
            matches!(result, Err(StartFailure::Failed(CalmError::Conflict(message)))
            if message == "ACP setup connection closed" || message == "ACP connection closed; dispatched request outcomes may be unknown")
        );
    }
}
