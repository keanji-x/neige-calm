//! Application-facing operations. Do not expose raw SDK creation/respawn:
//! on Unix those convenience calls can read the requesting process's entire
//! environment through /proc. Our creation request always supplies it explicitly.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use rmux_proto::{NewSessionExtRequest, ProcessCommand, Response, SessionName, TerminalSize};

/// Complete caller intent for a new, single-pane terminal session.
pub struct TerminalLaunchConfig {
    pub name: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Full permitted client environment, NOT overrides of the application environment.
    pub environment: BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("invalid terminal specification: {0}")]
    Invalid(&'static str),
    #[error("runtime rejected terminal creation")]
    Rejected,
    /// A request may have executed despite missing its response. The owner
    /// must reconcile this exact name; never retry or compensate blindly.
    #[error("terminal creation outcome is unknown for {name}")]
    OutcomeUnknown { name: String },
}

pub struct RuntimeClient {
    socket: PathBuf,
    timeout: Duration,
    sdk: rmux_sdk::Rmux,
}

impl RuntimeClient {
    pub(crate) fn new(socket: PathBuf, timeout: Duration, sdk: rmux_sdk::Rmux) -> Self {
        Self {
            socket,
            timeout,
            sdk,
        }
    }

    pub async fn create(
        &self,
        launch_config: TerminalLaunchConfig,
    ) -> Result<TerminalSession, CreateError> {
        let name = launch_config.name.clone();
        let request = create_request(launch_config)?;
        let endpoint = self.socket.clone();
        let request_name = name.clone();
        let unknown = || CreateError::OutcomeUnknown { name: name.clone() };
        // A timed-out blocking request may still finish. Preserve that as an
        // unknown outcome instead of reporting that no process was created.
        let attempt = tokio::task::spawn_blocking(move || {
            let mut client = rmux_client::connect(&endpoint).map_err(|_| CreateError::Rejected)?;
            match client.new_session_extended(request) {
                Ok(Response::NewSession(response))
                    if response.session_name.as_str() == request_name && response.detached =>
                {
                    Ok(())
                }
                Ok(Response::Error(_)) => Err(CreateError::Rejected),
                _ => Err(CreateError::OutcomeUnknown { name: request_name }),
            }
        });
        match tokio::time::timeout(self.timeout, attempt).await {
            Ok(Ok(Ok(()))) => self.attach_existing(&name).await.map_err(|_| unknown()),
            Ok(Ok(Err(CreateError::Rejected))) => Err(CreateError::Rejected),
            _ => Err(unknown()),
        }
    }

    /// Lookup only. Durable callers must separately validate their persisted
    /// runtime/pane generation before admitting actions; this does not respawn.
    pub async fn attach_existing(&self, name: &str) -> anyhow::Result<TerminalSession> {
        let session = self.sdk.session(SessionName::new(name)?).await?;
        let mut info = session.pane(0, 0).info().await?;
        anyhow::ensure!(info.panes.len() == 1, "expected one terminal pane");
        let id = info.panes.remove(0).id;
        let pane = session.pane_by_id(id).await?;
        Ok(TerminalSession { pane })
    }

    pub async fn list_sessions(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .sdk
            .list_sessions()
            .await?
            .into_iter()
            .map(|name| name.to_string())
            .collect())
    }

    pub async fn has_session(&self, name: &str) -> anyhow::Result<bool> {
        Ok(self.sdk.has_session(SessionName::new(name)?).await?)
    }

    /// Runtime-owner operation, never a Planner tool capability.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        Ok(self.sdk.shutdown().await?)
    }
}

/// A live SDK pane without the implicit-environment create/respawn surface.
pub struct TerminalSession {
    pane: rmux_sdk::Pane,
}

impl TerminalSession {
    pub async fn send_text(&self, text: &str) -> anyhow::Result<()> {
        Ok(self.pane.send_text(text).await?)
    }
    pub async fn send_key(&self, key: &str) -> anyhow::Result<()> {
        Ok(self.pane.send_key(key).await?)
    }
    pub async fn snapshot(&self) -> anyhow::Result<rmux_sdk::PaneSnapshot> {
        Ok(self.pane.snapshot().await?)
    }
    pub async fn wait_for_text(&self, text: &str) -> anyhow::Result<()> {
        Ok(self.pane.wait_for_text(text).await?)
    }
    pub async fn recover_output(&self) -> anyhow::Result<rmux_sdk::PaneRecoveryStream> {
        Ok(self.pane.recover_output().await?)
    }
    pub async fn wait_exit(&self) -> anyhow::Result<Option<rmux_sdk::PaneExitState>> {
        Ok(self.pane.wait_exit().await?)
    }
    pub async fn info(&self) -> anyhow::Result<rmux_sdk::PaneInfo> {
        let mut info = self.pane.info().await?;
        anyhow::ensure!(info.panes.len() == 1, "expected one terminal pane");
        Ok(info.panes.remove(0))
    }
    pub async fn close(self) -> anyhow::Result<()> {
        self.pane.close().await?;
        Ok(())
    }
}

fn create_request(
    launch_config: TerminalLaunchConfig,
) -> Result<NewSessionExtRequest, CreateError> {
    let name =
        SessionName::new(&launch_config.name).map_err(|_| CreateError::Invalid("session name"))?;
    if launch_config.argv.is_empty()
        || launch_config.argv[0].is_empty()
        || launch_config.argv.iter().any(|arg| arg.contains('\0'))
    {
        return Err(CreateError::Invalid(
            "argv must contain a program and no NUL bytes",
        ));
    }
    if !(1..=512).contains(&launch_config.cols) || !(1..=256).contains(&launch_config.rows) {
        return Err(CreateError::Invalid(
            "terminal geometry exceeds 512 columns or 256 rows",
        ));
    }
    let cwd = launch_config
        .cwd
        .to_str()
        .filter(|cwd| !cwd.contains('\0'))
        .filter(|_| launch_config.cwd.is_absolute())
        .ok_or(CreateError::Invalid("cwd must be an absolute UTF-8 path"))?;
    for (key, value) in &launch_config.environment {
        let mut chars = key.chars();
        if !chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            || value.contains('\0')
        {
            return Err(CreateError::Invalid("invalid environment assignment"));
        }
    }
    Ok(NewSessionExtRequest {
        session_name: Some(name),
        working_directory: Some(cwd.replace('#', "##")),
        detached: true,
        size: Some(TerminalSize {
            cols: launch_config.cols,
            rows: launch_config.rows,
        }),
        environment: Some(Vec::new()),
        group_target: None,
        attach_if_exists: false,
        detach_other_clients: false,
        kill_other_clients: false,
        flags: None,
        window_name: None,
        print_session_info: false,
        print_format: None,
        command: None,
        process_command: Some(ProcessCommand::Argv(launch_config.argv)),
        client_environment: Some(
            launch_config
                .environment
                .into_iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect(),
        ),
        skip_environment_update: true,
    })
}
