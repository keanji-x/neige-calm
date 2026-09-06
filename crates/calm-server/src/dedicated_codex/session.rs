use super::{
    Error, HomeReceipt, HomeSeed, NativeMcp, PrivateHome, Result, TurnAdmission, TurnLaunch,
    bootstrap, home, layout, policy,
};
use crate::codex_appserver::{
    ClientInfo, CodexAppServer, NotificationStream, PermissionThreadStartParams,
    ThreadPermissionSelection,
};
use async_trait::async_trait;
use calm_worker_runtime::{
    BoundaryHandle, BoundaryState, LaunchConfig, Mount, NetworkPolicy, QuiescenceProof, Runtime,
    RuntimeConfig,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct ControllerConfig {
    pub private_root: PathBuf,
    pub runtime: RuntimeConfig,
    pub codex_binary: PathBuf,
    pub mcp_shim: PathBuf,
    /// Inner Codex bwrap, explicitly selected independently from the owned runtime.
    pub sandbox_bwrap: PathBuf,
    pub provider_environment: BTreeMap<String, String>,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl std::fmt::Debug for ControllerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerConfig")
            .field("private_root", &self.private_root)
            .field("codex_binary", &self.codex_binary)
            .field("provider_environment", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DedicatedIdentity {
    pub run_id: String,
    pub attempt_id: String,
    pub card_id: String,
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DedicatedRequest {
    pub identity: DedicatedIdentity,
    pub workspace: PathBuf,
    pub developer_instructions: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedEndpoint {
    pub version: u32,
    pub request: DedicatedRequest,
    pub home: HomeReceipt,
    pub boundary: BoundaryHandle,
    launch: LaunchConfig,
}

impl PreparedEndpoint {
    /// Frozen submitted request for the workspace controller's actual runtime
    /// verification. Private data: transport environment may contain credentials.
    pub fn launch_request(&self) -> &LaunchConfig {
        &self.launch
    }
}

impl std::fmt::Debug for PreparedEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedEndpoint")
            .field("version", &self.version)
            .field("request", &self.request)
            .field("home", &self.home)
            .field("boundary", &self.boundary)
            .field("launch", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RequestPhase {
    Prepared,
    ProviderStarting,
    Connected,
    CreatingThread,
    ThreadReady {
        thread_id: String,
    },
    IssuingTurn {
        thread_id: String,
        request_key: String,
        prompt_digest: String,
    },
    TurnActive {
        thread_id: String,
        turn_id: String,
        request_key: String,
        prompt_digest: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StopState {
    Open,
    Requested,
    Quiesced(QuiescenceProof),
}

/// Physical request journal only. Store this through the existing Operation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub endpoint: PreparedEndpoint,
    pub phase: RequestPhase,
    pub stop: StopState,
}
impl SessionRecord {
    pub fn prepared(endpoint: PreparedEndpoint) -> Self {
        Self {
            endpoint,
            phase: RequestPhase::Prepared,
            stop: StopState::Open,
        }
    }
}

/// Mandatory compare-and-save callback. The owner atomically compares the expected
/// Operation record/lease, rechecks authorization, then stores next. A stale writer
/// must fail BEFORE issuing another external request; identical intent is not a grant.
#[async_trait]
pub trait Checkpoint: Send + Sync {
    async fn save(&self, expected: &SessionRecord, next: &SessionRecord) -> Result<()>;
}

pub struct Controller {
    runtime: Arc<Runtime>,
    private_home: PrivateHome,
    config: ControllerConfig,
    #[cfg(feature = "fixtures")]
    fixture_arguments: Option<Vec<String>>,
}

impl Controller {
    pub fn new(mut config: ControllerConfig) -> Result<Self> {
        if config.connect_timeout.is_zero()
            || config.connect_timeout > Duration::from_secs(60)
            || config.request_timeout.is_zero()
            || config.request_timeout > Duration::from_secs(60)
        {
            return Err(Error::Configuration(
                "provider timeouts must be within (0,60s]".into(),
            ));
        }
        // Refuse readable aliases before even creating a credential directory.
        layout::private_sources(&mut config)?;
        let private_home = PrivateHome::open(&config.private_root)?;
        config.private_root = config.private_root.canonicalize()?;
        let runtime = Arc::new(Runtime::new(config.runtime.clone())?);
        Ok(Self {
            runtime,
            private_home,
            config,
            #[cfg(feature = "fixtures")]
            fixture_arguments: None,
        })
    }

    /// Uses the integration test executable as a fake provider process; no extra
    /// fake-provider production binary or alternate production endpoint is shipped.
    #[cfg(feature = "fixtures")]
    pub fn with_fixture_arguments(mut self, arguments: Vec<String>) -> Self {
        self.fixture_arguments = Some(arguments);
        self
    }

    pub async fn prepare(
        &self,
        request: DedicatedRequest,
        seed: &HomeSeed,
        native: &NativeMcp,
    ) -> Result<PreparedEndpoint> {
        home::valid_segment(&request.identity.run_id)?;
        for value in [
            &request.identity.attempt_id,
            &request.identity.card_id,
            &request.identity.session_id,
        ] {
            if value.trim().is_empty() {
                return Err(Error::Configuration(
                    "complete attempt/card/session identity required".into(),
                ));
            }
        }
        let workspace = request.workspace.canonicalize()?;
        if workspace.starts_with(&self.config.private_root)
            || self.config.private_root.starts_with(&workspace)
        {
            return Err(Error::Configuration(
                "private provider home must be outside workspace".into(),
            ));
        }
        bootstrap::workspace_and_tools(&self.config, &workspace)?;
        bootstrap::helper_capabilities(&self.config.sandbox_bwrap).await?;
        let request_digest = home::digest(
            &serde_json::to_vec(&request)
                .map_err(|_| Error::Configuration("request cannot be encoded".into()))?,
        );
        let receipt =
            self.private_home
                .prepare(&request.identity.run_id, &request_digest, seed, native)?;
        let socket_name = receipt
            .mcp_source_socket
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::Configuration("MCP socket name required".into()))?;
        let arguments = vec![
            "app-server".into(),
            "--listen".into(),
            format!("unix://{}/app-server.sock", policy::PROVIDER_CONTROL),
        ];
        #[cfg(feature = "fixtures")]
        let arguments = self.fixture_arguments.clone().unwrap_or(arguments);
        let mut launch = LaunchConfig {
            attempt_id: request.identity.attempt_id.clone(),
            network: NetworkPolicy::Provider,
            workspace,
            program: "/provider-bin/codex".into(),
            args: arguments,
            environment: home::provider_environment(&self.config.provider_environment)?,
            mounts: vec![
                Mount {
                    source: receipt.home.clone(),
                    destination: policy::PROVIDER_HOME.into(),
                    writable: true,
                },
                Mount {
                    source: receipt.control.clone(),
                    destination: policy::PROVIDER_CONTROL.into(),
                    writable: true,
                },
                Mount {
                    source: receipt.mcp_source_socket.clone(),
                    destination: format!("{}/{socket_name}", policy::PROVIDER_MCP).into(),
                    writable: false,
                },
            ],
        };
        launch
            .mounts
            .extend(bootstrap::executable_mounts(&self.config));
        // Explicit provider-only DNS/TLS inputs; no broad /etc or host-home mount.
        for path in [
            "/etc/resolv.conf",
            "/etc/hosts",
            "/etc/nsswitch.conf",
            "/etc/ssl/certs",
        ] {
            if !std::path::Path::new(path).try_exists()? {
                return Err(Error::Unsupported(format!(
                    "provider transport file unavailable: {path}"
                )));
            }
            launch.mounts.push(Mount {
                source: path.into(),
                destination: path.into(),
                writable: false,
            });
        }
        // Preserve system requirement authority without importing executable system
        // config/MCP entries. Missing optional policy files do not synthesize policy.
        for path in [
            "/etc/codex/requirements.toml",
            "/etc/codex/managed_config.toml",
        ] {
            if std::path::Path::new(path).try_exists()? {
                launch.mounts.push(Mount {
                    source: path.into(),
                    destination: path.into(),
                    writable: false,
                });
            }
        }
        let runtime = self.runtime.clone();
        let run_id = request.identity.run_id.clone();
        let submitted_launch = launch.clone();
        let boundary = tokio::task::spawn_blocking(move || runtime.prepare(&run_id, &launch))
            .await
            .map_err(|_| Error::Unknown("boundary preparation interrupted".into()))??;
        Ok(PreparedEndpoint {
            version: 2,
            request,
            home: receipt,
            boundary,
            launch: submitted_launch,
        })
    }

    pub async fn probe(&self, endpoint: &PreparedEndpoint) -> Result<BoundaryState> {
        self.validate(endpoint)?;
        let runtime = self.runtime.clone();
        let handle = endpoint.boundary.clone();
        tokio::task::spawn_blocking(move || runtime.probe(&handle))
            .await
            .map_err(|_| Error::Unknown("boundary probe interrupted".into()))?
            .map_err(Into::into)
    }

    pub async fn connect(
        &self,
        mut record: SessionRecord,
        checkpoint: &dyn Checkpoint,
    ) -> Result<Session> {
        self.validate(&record.endpoint)?;
        if record.endpoint.version != 2 {
            return Err(Error::Unsupported(
                "endpoint predates required sandbox bootstrap; activation refused".into(),
            ));
        }
        if record.stop != StopState::Open {
            return Err(Error::Conflict("provider admission is closed".into()));
        }
        PrivateHome::verify(&record.endpoint.home)?;
        match (&record.phase, self.probe(&record.endpoint).await?) {
            (RequestPhase::Prepared | RequestPhase::ProviderStarting, BoundaryState::Prepared) => {
                let expected = record.clone();
                record.phase = RequestPhase::ProviderStarting;
                checkpoint.save(&expected, &record).await?;
                let runtime = self.runtime.clone();
                let handle = record.endpoint.boundary.clone();
                tokio::task::spawn_blocking(move || runtime.start(&handle))
                    .await
                    .map_err(|_| Error::Unknown("boundary start interrupted".into()))??;
            }
            (_, BoundaryState::Running) => {}
            _ => {
                return Err(Error::Unknown(
                    "bound provider is not available; no replacement started".into(),
                ));
            }
        }
        let until = tokio::time::Instant::now() + self.config.connect_timeout;
        let (client, notifications) = loop {
            let remaining = until.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::Unknown(
                    "dedicated endpoint did not become ready".into(),
                ));
            }
            match tokio::time::timeout(remaining, layout::connect(&record.endpoint.home)).await {
                Ok(Ok(pair)) => break pair,
                _ => {
                    if !matches!(self.probe(&record.endpoint).await?, BoundaryState::Running) {
                        return Err(Error::Unknown("provider exited before connection".into()));
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        let client = client.with_request_timeout(self.config.request_timeout);
        client
            .initialize(ClientInfo {
                name: "neige-dedicated-worker".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            })
            .await
            .map_err(|_| Error::Unknown("provider initialize failed".into()))?;
        let mut session = Session {
            runtime: self.runtime.clone(),
            client: Arc::new(client),
            notifications: Some(notifications),
            record,
        };
        session.require_profile().await?;
        if matches!(
            session.record.phase,
            RequestPhase::Prepared | RequestPhase::ProviderStarting
        ) {
            session
                .transition(RequestPhase::Connected, checkpoint)
                .await?;
        }
        Ok(session)
    }

    pub async fn stop(
        &self,
        record: &mut SessionRecord,
        checkpoint: &dyn Checkpoint,
        deadline: Duration,
    ) -> Result<BoundaryState> {
        self.validate(&record.endpoint)?;
        if let StopState::Quiesced(proof) = &record.stop {
            if proof.handle != record.endpoint.boundary {
                return Err(Error::Conflict(
                    "stop proof belongs to another endpoint".into(),
                ));
            }
            return Ok(BoundaryState::Quiesced(proof.clone()));
        }
        let mut next = record.clone();
        next.stop = StopState::Requested;
        checkpoint.save(record, &next).await?;
        *record = next;
        let runtime = self.runtime.clone();
        let handle = record.endpoint.boundary.clone();
        let state = tokio::task::spawn_blocking(move || runtime.stop(&handle, deadline))
            .await
            .map_err(|_| Error::Unknown("boundary stop interrupted".into()))??;
        if let BoundaryState::Quiesced(proof) = &state {
            let mut next = record.clone();
            next.stop = StopState::Quiesced(proof.clone());
            checkpoint.save(record, &next).await?;
            *record = next;
        }
        Ok(state)
    }

    fn validate(&self, endpoint: &PreparedEndpoint) -> Result<()> {
        home::valid_segment(&endpoint.request.identity.run_id)?;
        if !matches!(endpoint.version, 1 | 2)
            || endpoint.launch.attempt_id != endpoint.request.identity.attempt_id
            || endpoint.request.identity.run_id != endpoint.boundary.run_id
            || endpoint.request.identity.attempt_id != endpoint.boundary.attempt_id
            || endpoint.home.root
                != self
                    .config
                    .private_root
                    .join(&endpoint.request.identity.run_id)
            || endpoint.home.run_id != endpoint.boundary.run_id
            || endpoint.home.request_digest
                != home::digest(
                    &serde_json::to_vec(&endpoint.request)
                        .map_err(|_| Error::Configuration("request cannot be encoded".into()))?,
                )
        {
            return Err(Error::Conflict(
                "dedicated endpoint identity mismatch".into(),
            ));
        }
        Ok(())
    }
}

pub struct Session {
    runtime: Arc<Runtime>,
    client: Arc<CodexAppServer>,
    notifications: Option<NotificationStream>,
    record: SessionRecord,
}
impl Session {
    /// Explicit fixture-only zero-model policy probe; no thread/turn may exist.
    #[cfg(feature = "fixtures")]
    pub async fn command_for_fixture(
        &self,
        command: Vec<String>,
        environment: BTreeMap<String, String>,
    ) -> Result<serde_json::Value> {
        if self.record.phase != RequestPhase::Connected {
            return Err(Error::Conflict(
                "command probe requires a fresh connected endpoint".into(),
            ));
        }
        self.ready_for_request().await?;
        self.client
            .command_exec_for_fixture(command, environment)
            .await
            .map_err(|error| Error::Unknown(format!("fixture command control failed: {error}")))
    }

    pub fn record(&self) -> &SessionRecord {
        &self.record
    }
    pub fn take_notifications(&mut self) -> Result<NotificationStream> {
        self.notifications
            .take()
            .ok_or_else(|| Error::Conflict("notification stream already owned by caller".into()))
    }

    async fn require_profile(&self) -> Result<()> {
        PrivateHome::verify(&self.record.endpoint.home)?;
        let mut cursor = None;
        let mut seen = BTreeSet::new();
        for _ in 0..16 {
            let page = self
                .client
                .permission_profile_list(policy::WORKSPACE, cursor.as_deref())
                .await
                .map_err(|_| Error::Unsupported("named permission profiles unavailable".into()))?;
            if let Some(profile) = page
                .data
                .iter()
                .find(|profile| profile.id == policy::DELIVERY_PROFILE)
            {
                return if profile.allowed {
                    Ok(())
                } else {
                    Err(Error::Unsupported(
                        "delivery profile disallowed by requirements".into(),
                    ))
                };
            }
            match page.next_cursor {
                Some(next) if seen.insert(next.clone()) => cursor = Some(next),
                _ => {
                    return Err(Error::Unsupported(
                        "required delivery permission profile missing".into(),
                    ));
                }
            }
        }
        Err(Error::Unsupported(
            "permission profile pagination did not converge".into(),
        ))
    }

    async fn ready_for_request(&self) -> Result<()> {
        if self.record.stop != StopState::Open {
            return Err(Error::Conflict("provider admission closed".into()));
        }
        let runtime = self.runtime.clone();
        let handle = self.record.endpoint.boundary.clone();
        let state = tokio::task::spawn_blocking(move || runtime.probe(&handle))
            .await
            .map_err(|_| Error::Unknown("boundary probe interrupted".into()))??;
        if !matches!(state, BoundaryState::Running) {
            return Err(Error::Unknown("bound provider is no longer running".into()));
        }
        self.require_profile().await
    }

    async fn transition(&mut self, phase: RequestPhase, checkpoint: &dyn Checkpoint) -> Result<()> {
        let mut next = self.record.clone();
        next.phase = phase;
        checkpoint.save(&self.record, &next).await?;
        self.record = next;
        Ok(())
    }

    pub async fn create_thread(&mut self, checkpoint: &dyn Checkpoint) -> Result<String> {
        if let RequestPhase::ThreadReady { thread_id } = &self.record.phase {
            return Ok(thread_id.clone());
        }
        if self.record.phase != RequestPhase::Connected {
            return Err(Error::Unknown(
                "thread request requires reconciliation; not repeated".into(),
            ));
        }
        self.ready_for_request().await?;
        self.transition(RequestPhase::CreatingThread, checkpoint)
            .await?;
        let thread = self
            .client
            .thread_start_with_permissions(PermissionThreadStartParams {
                cwd: policy::WORKSPACE.into(),
                approval_policy: "never".into(),
                permissions: ThreadPermissionSelection::NamedProfile(
                    policy::DELIVERY_PROFILE.into(),
                ),
                developer_instructions: Some(
                    self.record.endpoint.request.developer_instructions.clone(),
                ),
                config: None,
            })
            .await
            .map_err(|_| {
                Error::Unknown("thread creation reply unavailable; reconcile this endpoint".into())
            })?;
        let id = thread
            .thread_id()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Unknown("thread creation omitted identity".into()))?
            .to_string();
        if thread.thread.get("cwd").and_then(serde_json::Value::as_str) != Some(policy::WORKSPACE)
            || !thread
                .thread
                .get("turns")
                .and_then(serde_json::Value::as_array)
                .is_some_and(Vec::is_empty)
        {
            return Err(Error::Unknown(
                "created thread context does not match this endpoint".into(),
            ));
        }
        self.transition(
            RequestPhase::ThreadReady {
                thread_id: id.clone(),
            },
            checkpoint,
        )
        .await
        .map_err(|_| {
            Error::Unknown("thread was created but its acknowledgement checkpoint failed".into())
        })?;
        Ok(id)
    }

    pub async fn begin_turn(
        &mut self,
        request_key: &str,
        prompt: &str,
        checkpoint: &dyn Checkpoint,
        admission: &dyn TurnAdmission,
    ) -> Result<String> {
        if request_key.is_empty() || prompt.is_empty() {
            return Err(Error::Configuration("turn key and prompt required".into()));
        }
        let digest = home::digest(prompt.as_bytes());
        if let RequestPhase::TurnActive {
            request_key: old,
            prompt_digest,
            turn_id,
            ..
        } = &self.record.phase
        {
            return if old == request_key && *prompt_digest == digest {
                Ok(turn_id.clone())
            } else {
                Err(Error::Conflict(
                    "turn already bound to another request".into(),
                ))
            };
        }
        let RequestPhase::ThreadReady { thread_id } = &self.record.phase else {
            return Err(Error::Unknown(
                "turn may already have been issued; no automatic replay".into(),
            ));
        };
        let thread_id = thread_id.clone();
        self.ready_for_request().await?;
        self.transition(
            RequestPhase::IssuingTurn {
                thread_id: thread_id.clone(),
                request_key: request_key.into(),
                prompt_digest: digest.clone(),
            },
            checkpoint,
        )
        .await?;
        // The CAS is committed above. Only bounded control I/O enters the caller's
        // final TaskLaunch guard; the acknowledgement checkpoint below is outside.
        let turn = admission
            .admit(TurnLaunch::new(
                self.client.clone(),
                self.record.endpoint.clone(),
                thread_id.clone(),
                request_key.into(),
                prompt.into(),
            ))
            .await?;
        let turn_id = turn
            .turn_id()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Unknown("turn reply omitted identity".into()))?
            .to_string();
        self.transition(
            RequestPhase::TurnActive {
                thread_id,
                turn_id: turn_id.clone(),
                request_key: request_key.into(),
                prompt_digest: digest,
            },
            checkpoint,
        )
        .await
        .map_err(|_| {
            Error::Unknown("turn was issued but its acknowledgement checkpoint failed".into())
        })?;
        Ok(turn_id)
    }

    /// Only this endpoint's single thread can be adopted. An unacknowledged turn
    /// remains Unknown rather than guessing a latest turn or replaying the prompt.
    pub async fn reconcile(&mut self, checkpoint: &dyn Checkpoint) -> Result<()> {
        if matches!(self.record.phase, RequestPhase::IssuingTurn { .. }) {
            return Err(Error::Unknown(
                "turn acknowledgement was not persisted; explicit reconciliation required".into(),
            ));
        }
        self.ready_for_request().await?;
        let ids = self
            .client
            .thread_loaded_list()
            .await
            .map_err(|_| Error::Unknown("owned thread inventory unavailable".into()))?;
        let expected = match &self.record.phase {
            RequestPhase::CreatingThread if ids.len() == 1 => ids[0].clone(),
            RequestPhase::ThreadReady { thread_id }
            | RequestPhase::TurnActive { thread_id, .. }
                if ids.len() == 1 && ids.first() == Some(thread_id) =>
            {
                thread_id.clone()
            }
            RequestPhase::Connected if ids.is_empty() => return Ok(()),
            _ => {
                return Err(Error::Unknown(
                    "owned endpoint thread identity ambiguous".into(),
                ));
            }
        };
        let thread = self
            .client
            .thread_read_full(&expected)
            .await
            .map_err(|_| Error::Unknown("owned thread details unavailable".into()))?;
        if thread.thread_id() != Some(expected.as_str())
            || thread.thread.get("cwd").and_then(serde_json::Value::as_str)
                != Some(policy::WORKSPACE)
        {
            return Err(Error::Unknown("owned thread context mismatch".into()));
        }
        let turns = thread
            .thread
            .get("turns")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| Error::Unknown("owned thread has no turn inventory".into()))?;
        match &self.record.phase {
            RequestPhase::TurnActive { turn_id, .. }
                if turns.len() == 1
                    && turns[0].get("id").and_then(serde_json::Value::as_str) == Some(turn_id) => {}
            RequestPhase::ThreadReady { .. } | RequestPhase::CreatingThread if turns.is_empty() => {
            }
            _ => {
                return Err(Error::Unknown(
                    "owned thread execution inventory changed".into(),
                ));
            }
        }
        if self.record.phase == RequestPhase::CreatingThread {
            if !thread
                .thread
                .get("turns")
                .and_then(serde_json::Value::as_array)
                .is_some_and(Vec::is_empty)
            {
                return Err(Error::Unknown(
                    "unbound thread already has execution history".into(),
                ));
            }
            self.transition(
                RequestPhase::ThreadReady {
                    thread_id: expected,
                },
                checkpoint,
            )
            .await
            .map_err(|_| Error::Unknown("owned thread acknowledgement checkpoint failed".into()))?;
        } else {
            let resumed = self
                .client
                .thread_resume_named(&expected, policy::DELIVERY_PROFILE)
                .await
                .map_err(|_| Error::Unknown("exact owned thread could not resume".into()))?;
            if resumed.thread_id() != Some(expected.as_str())
                || resumed
                    .thread
                    .get("cwd")
                    .and_then(serde_json::Value::as_str)
                    != Some(policy::WORKSPACE)
            {
                return Err(Error::Unknown("resumed thread context mismatch".into()));
            }
        }
        Ok(())
    }
}
