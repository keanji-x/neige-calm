use super::config::{AcpPlannerHost, Registration};
use crate::error::{CalmError, Result};
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
    pub commands: Commands,
    #[cfg(all(test, feature = "fixtures"))]
    stop_scope: String,
}
impl Process {
    pub async fn spawn(
        host: &AcpPlannerHost,
        registration: Registration<'_>,
        worker: &str,
        cwd: &std::path::Path,
    ) -> Result<Self> {
        Self::spawn_control(host, registration, worker, cwd)
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
    ) -> std::result::Result<Self, StartFailure> {
        let Registration { config, profile } = registration;
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .args(profile.argv(None))
            .current_dir(cwd)
            .env_clear()
            .envs(&config.env)
            .env(
                "PATH",
                crate::kernel_bin_path::kernel_led_path()
                    .map_err(CalmError::from)?
                    .path,
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
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
        let connect = if profile.shared_server {
            Connection::new_shared
        } else {
            Connection::new
        };
        let connection = connect(
            child
                .stdout()
                .ok_or_else(|| CalmError::Internal("ACP stdout missing".into()))?,
            child
                .stdin()
                .ok_or_else(|| CalmError::Internal("ACP stdin missing".into()))?,
        );
        match negotiate(&connection.client, registration, false).await {
            Ok(capabilities) => Ok(Self {
                child,
                connection,
                capabilities,
                commands: Commands::default(),
                #[cfg(all(test, feature = "fixtures"))]
                stop_scope: host.agent_logs.to_string_lossy().into_owned(),
            }),
            Err(refusal) => {
                connection.client.close();
                let _ = child.start_kill();
                let _ = reap(&mut child).await;
                Err(StartFailure::Failed(CalmError::Conflict(refusal)))
            }
        }
    }
    pub async fn stop(mut self) -> Result<()> {
        self.connection.client.close();
        self.child.start_kill()?;
        #[cfg(all(test, feature = "fixtures"))]
        crate::test_seams::operation_fault("acp-control-reap", &self.stop_scope)?;
        reap(&mut self.child).await
    }
}

/// The most of one agent process's stderr that is kept; the rest is read and dropped, so the agent
/// never blocks writing it.
const STDERR_LIMIT: usize = 256 * 1024;

/// The file the stderr of `worker`'s agent processes is kept in, under the host's data directory;
/// each launch overwrites it.
pub(super) fn worker_log_name(worker: &str) -> String {
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
    if name.is_empty() { "_".into() } else { name }
}

fn stderr_log(host: &AcpPlannerHost, worker: &str) -> std::path::PathBuf {
    host.agent_logs
        .join(format!("{}.stderr.log", worker_log_name(worker)))
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
pub(super) async fn negotiate(
    client: &provider::acp::Client,
    Registration { config, profile }: Registration<'_>,
    compaction: bool,
) -> std::result::Result<protocol::AgentCapabilities, String> {
    let response = protocol::initialize_with_compaction(
        client,
        "neige",
        env!("CARGO_PKG_VERSION"),
        compaction,
    )
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

async fn reap(child: &mut GroupChild) -> Result<()> {
    // Cancellation/timeout leaves GroupChild armed. A cleanup attempt alone is
    // not exit evidence for canonical Keep or generation retirement.
    let (status, group) =
        tokio::time::timeout(Duration::from_secs(3), child.wait_and_release_group())
            .await
            .map_err(|_| {
                CalmError::Conflict("ACP process exit is unconfirmed after stop deadline".into())
            })?;
    group.sweep();
    status.map(drop).map_err(Into::into)
}

pub(super) async fn setup_request(
    connection: &mut Connection,
    commands: &Commands,
    method: &str,
    params: Value,
    gate: &provider::send_boundary::PreSendGate<'_>,
) -> std::result::Result<Value, StartFailure> {
    let response = connection
        .client
        .submit_gated(method, params, gate)
        .await
        .map_err(|error| StartFailure::Failed(CalmError::Conflict(error.to_string())))?;
    let response = response.wait(Duration::from_secs(10));
    tokio::pin!(response);
    let mut incoming_open = true;
    loop {
        tokio::select! {
            result=&mut response=>return finish_setup(&connection.client,&mut connection.incoming,commands,result).await,
            incoming=connection.incoming.recv(),if incoming_open=>match incoming {
                Some(Incoming::Request{id,method,..})=>refuse(&connection.client,id,&method).await.map_err(|error| StartFailure::Failed(CalmError::Conflict(error.to_string())))?,
                Some(frame @ Incoming::Notification{..})=>commands.observe(&frame), // Only the command catalog survives setup.

                None=>incoming_open=false,
            }
        }
    }
}
/// Session-scoped declarations observed from setup, idle traffic and the routed driver.
/// Replacing a menu replaces that session's catalog, including an empty menu.
#[derive(Clone, Default)]
pub(super) struct Commands(
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>>,
);
impl Commands {
    pub(super) fn contains(&self, session: &str, command: &str) -> bool {
        self.0
            .lock()
            .expect("ACP commands")
            .get(session)
            .is_some_and(|names| names.iter().any(|name| name == command))
    }
    pub(super) fn declared(&self, session: &str) -> bool {
        self.0.lock().expect("ACP commands").contains_key(session)
    }
    pub(super) fn retain_sessions(&self, sessions: &std::collections::HashSet<&str>) {
        self.0
            .lock()
            .expect("ACP commands")
            .retain(|id, _| sessions.contains(id.as_str()));
    }
    pub(super) fn observe_for(&self, session: &str, frame: &Incoming) {
        if let Incoming::Notification { params, .. } = frame
            && params["sessionId"].as_str() == Some(session)
        {
            self.observe(frame);
        }
    }
    pub(super) fn forget(&self, session: &str) {
        self.0.lock().expect("ACP commands").remove(session);
    }
    pub(super) fn observe(&self, frame: &Incoming) {
        let Incoming::Notification { method, params } = frame else {
            return;
        };
        if method != "session/update"
            || params["update"]["sessionUpdate"] != "available_commands_update"
        {
            return;
        }
        let Some(session) = params["sessionId"].as_str() else {
            return;
        };
        let names = params["update"]["availableCommands"]
            .as_array()
            .map(|available| {
                available
                    .iter()
                    .filter_map(|command| command["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        self.0
            .lock()
            .expect("ACP commands")
            .insert(session.to_owned(), names);
    }
}

async fn finish_setup(
    client: &provider::acp::Client,
    incoming: &mut tokio::sync::mpsc::Receiver<Incoming>,
    commands: &Commands,
    result: std::result::Result<Value, provider::acp::Error>,
) -> std::result::Result<Value, StartFailure> {
    // The transport delivered these frames before the response. Drain replay at this boundary,
    // even when select chooses the ready response before the incoming branch.
    let limit = if client.is_shared() {
        incoming.max_capacity()
    } else {
        usize::MAX
    };
    for _ in 0..limit {
        let Ok(frame) = incoming.try_recv() else {
            break;
        };
        commands.observe(&frame);
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
    async fn control_reap_timeout_is_not_confirmed_exit() {
        let mut command = Command::new("/bin/sleep");
        command.arg("30").env_clear().kill_on_drop(true);
        set_process_group_leader(&mut command);
        let mut child = spawn_within(
            command,
            tokio::time::Instant::now() + Duration::from_secs(10),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            reap(&mut child).await,
            Err(CalmError::Conflict(_))
        ));
        child.kill_group();
        reap(&mut child).await.unwrap();
    }

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
            let result = finish_setup(
                &connection.client,
                &mut connection.incoming,
                &Commands::default(),
                Err(error),
            )
            .await;
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
        finish_setup(
            &connection.client,
            &mut connection.incoming,
            &Commands::default(),
            result,
        )
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

    struct Peer {
        connection: Connection,
        execution: crate::test_support::execution::Fixture,
        repo: crate::db::sqlite::SqlxRepo,
        id: workspace_runtime::execution::ExecutionId,
        _root: tempfile::TempDir,
    }
    // Real Engine stdin/output and the production setup pump; the peer controls frames.
    async fn peer(script: &str) -> Peer {
        use workspace_runtime::execution::test_support;
        let root = tempfile::tempdir().unwrap();
        let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let execution = crate::test_support::execution::Fixture::new(repo.db().clone()).await;
        let mut intent = test_support::intent(root.path().to_owned());
        intent.metadata.consumer = crate::acp_planner::execution::CONSUMER.into();
        intent.metadata.program.program = "/usr/bin/python3".into();
        intent.metadata.program.args = vec!["-u".into(), "-c".into(), script.into()];
        intent.metadata.program.replay_bytes = 4 * 1024 * 1024;
        let id = intent.metadata.id.clone();
        let write_db = (repo.db()).clone();
        write_db
            .writer()
            .with_writer(|write_session| {
                Box::pin(async move {
                    let mut tx = write_session.begin_immediate().await.unwrap();
                    let ticket = execution.engine.admit(&mut tx, intent).await.unwrap();
                    tx.commit().await.unwrap();
                    execution.engine.drive(ticket.committed());
                    crate::provider_execution::release::wait_spawned(&execution.engine, &id)
                        .await
                        .unwrap();
                    let connection = Connection::new(
                        crate::provider_execution::stdio::EngineStdio::new(
                            (*execution.engine).clone(),
                            id.clone(),
                        ),
                        crate::provider_execution::stdio::EngineStdio::new(
                            (*execution.engine).clone(),
                            id.clone(),
                        ),
                    );
                    Peer {
                        connection,
                        execution,
                        repo,
                        id,
                        _root: root,
                    }
                })
            })
            .await
            .expect("independent write operation")
    }
    async fn cleanup(process: &mut Peer) {
        use workspace_runtime::execution::Desired;
        process.connection.client.close();
        let write_db = (process.repo.db()).clone();
        write_db
            .writer()
            .with_writer(|write_session| {
                Box::pin(async move {
                    let mut tx = write_session.begin_immediate().await.unwrap();
                    let ticket = process
                        .execution
                        .engine
                        .request(&mut tx, &process.id, Desired::Stop)
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                    process.execution.engine.drive(ticket.committed());
                    crate::provider_execution::wait_settled(
                        &process.execution.engine,
                        &process.id,
                        Duration::from_secs(12),
                    )
                    .await
                    .unwrap();
                })
            })
            .await
            .expect("independent write operation")
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
            &mut process.connection,
            &Commands::default(),
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
            &mut process.connection,
            &Commands::default(),
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
            &mut process.connection,
            &Commands::default(),
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
            &mut process.connection,
            &Commands::default(),
            "session/set_config_option",
            protocol::encode_set_config_option("native", "model", "chosen"),
            &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
        )
        .await;
        cleanup(&mut process).await;
        assert!(
            matches!(result, Err(StartFailure::Failed(CalmError::Conflict(message)))
            if message == "ACP peer output reached EOF")
        );
    }
    #[tokio::test]
    async fn setup_pump_protocol_disconnect_retains_original_cause() {
        let mut process =
            peer("import sys\nsys.stdin.readline()\nprint('not-json secret-frame', flush=True)\n")
                .await;
        let result = setup_request(
            &mut process.connection,
            &Commands::default(),
            "session/load",
            serde_json::json!({"sessionId":"native"}),
            &provider::send_boundary::PreSendGate::new(|| Box::pin(async { Ok(()) })),
        )
        .await;
        cleanup(&mut process).await;
        assert!(
            matches!(result, Err(StartFailure::Failed(CalmError::Conflict(message)))
            if message == "ACP protocol violation: invalid JSON frame")
        );
    }
}

/// A menu may follow the setup result. Consume only declarations owned by this process;
/// requests during setup/maintenance never receive tool authority.
pub(super) async fn wait_command_catalog(
    connection: &mut Connection,
    commands: &Commands,
    native: &str,
    owns: impl Fn(&str) -> bool,
) -> std::result::Result<(), StartFailure> {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !commands.declared(native) {
            let frame = connection.incoming.recv().await.ok_or_else(|| {
                StartFailure::Failed(CalmError::Conflict(
                    "ACP command catalog was not declared".into(),
                ))
            })?;
            if let Incoming::Notification { params, .. } = &frame
                && params["sessionId"].as_str().is_some_and(&owns)
            {
                commands.observe(&frame);
            }
            if let Incoming::Request { id, method, .. } = frame {
                refuse(&connection.client, id, &method)
                    .await
                    .map_err(|error| {
                        StartFailure::Failed(CalmError::Conflict(error.to_string()))
                    })?;
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| {
        StartFailure::Failed(CalmError::Conflict("ACP command catalog timed out".into()))
    })?
}
