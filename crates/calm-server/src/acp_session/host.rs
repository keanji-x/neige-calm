use super::profile::AcpAgentProfile;
use crate::error::{CalmError, Result};
use crate::planner_process::MarkerInstance;
use calm_types::harness::PlannerPermissionMode;
use calm_types::runtime::{AgentProtocol, AgentProvider};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MARKER_KEY: &str = "NEIGE_ACP_PLANNER";
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcpPlannerConfig {
    pub agents: Vec<AcpAgentConfig>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcpAgentConfig {
    pub provider: AgentProvider,
    pub command: PathBuf,
    /// Arguments before the provider profile's ACP entry, such as a wrapper's or interpreter's
    /// own; the profile adds the agent's ACP subcommand and options after them.
    pub args: Vec<String>,
    /// Complete operator-declared environment additions. Nothing is inherited implicitly.
    pub env: BTreeMap<String, String>,
    /// The lowest agent version a launch accepts; the agent's profile says where it reports it.
    #[serde(deserialize_with = "semver_text")]
    pub min_agent_version: semver::Version,
    /// Permission policy for newly created Planner cards; omission keeps `never`.
    #[serde(default, deserialize_with = "declared_permission_mode")]
    pub default_permission_mode: Option<PlannerPermissionMode>,
}

// A present value must be a mode string: explicit null is invalid too.
fn declared_permission_mode<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> Result<Option<PlannerPermissionMode>, D::Error> {
    PlannerPermissionMode::deserialize(input).map(Some)
}

/// A semver version from its text.
fn semver_text<'de, D: serde::Deserializer<'de>>(input: D) -> Result<semver::Version, D::Error> {
    let text = String::deserialize(input)?;
    semver::Version::parse(&text)
        .map_err(|error| serde::de::Error::custom(format!("`min_agent_version` {text:?}: {error}")))
}

/// What a bound native session depends on: the agent process a launch talks to, and the
/// directory it runs in. A conversation resumes its native session only under the same key.
/// Admission policy such as `min_agent_version` is checked on every launch instead, so raising it
/// resets nothing.
#[derive(Serialize)]
struct NativeSessionKey<'a> {
    provider: &'a AgentProvider,
    command: &'a Path,
    args: &'a [String],
    env: &'a BTreeMap<String, String>,
    cwd: &'a Path,
}

impl AcpAgentConfig {
    /// The digest a bound conversation's native session is claimed under for `cwd`.
    pub(crate) fn native_session_digest(&self, cwd: &Path) -> Result<String> {
        use sha2::Digest as _;
        // Exhaustive, so a new registration field has to be placed in or out of the key here.
        let Self {
            provider,
            command,
            args,
            env,
            min_agent_version: _,
            default_permission_mode: _, // Creation policy never changes native-session identity.
        } = self;
        let key = NativeSessionKey {
            provider,
            command,
            args,
            env,
            cwd,
        };
        Ok(hex::encode(sha2::Sha256::digest(serde_json::to_vec(&key)?)))
    }
}

/// Registration fields this version no longer reads, and what replaced each: a file that fails
/// to parse and still names one is told what to change, beside the parse error. Delete this list
/// and [`replaced_field_hint`] in the first release after the one that ships them (#2924): every
/// operator file has been rewritten by then, and the parse error alone names the unknown field.
const REPLACED_FIELDS: [(&str, &str); 2] = [
    (
        "expected_agent_name",
        "remove it: the provider's profile fixes the agent name it accepts",
    ),
    (
        "expected_agent_version",
        "rename it to `min_agent_version`: the lowest semver version accepted",
    ),
];

/// What to change in a registration that failed to parse, when it still names replaced fields.
fn replaced_field_hint(bytes: &[u8]) -> Option<String> {
    let raw: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let agents = raw.get("agents")?.as_array()?;
    let changes: Vec<String> = REPLACED_FIELDS
        .into_iter()
        .filter(|(field, _)| agents.iter().any(|agent| agent.get(field).is_some()))
        .map(|(field, change)| format!("ACP registration field `{field}` was replaced; {change}"))
        .collect();
    (!changes.is_empty()).then(|| {
        format!(
            "{}. `args` no longer holds the ACP subcommand: the provider's profile adds it, so \
             drop `\"acp\"` from `args`",
            changes.join(". ")
        )
    })
}

impl AcpPlannerConfig {
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let config: Self =
            serde_json::from_slice(&bytes).map_err(|error| match replaced_field_hint(&bytes) {
                Some(hint) => CalmError::BadRequest(format!("{hint} ({error})")),
                None => error.into(),
            })?;
        config.validate()?;
        Ok(config)
    }
    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for agent in &self.agents {
            let profile = AcpAgentProfile::of(&agent.provider)
                .filter(|_| agent.provider.protocol() == AgentProtocol::Acp)
                .filter(|_| seen.insert(agent.provider.wire_name()))
                .ok_or_else(|| {
                    CalmError::BadRequest(
                        "ACP registration names an unsupported or duplicate agent".into(),
                    )
                })?;
            if !agent.command.is_absolute() {
                return Err(CalmError::BadRequest(
                    "ACP registration requires an absolute command".into(),
                ));
            }
            if agent
                .env
                .keys()
                .any(|key| key == "PATH" || key.starts_with("NEIGE_") || key.contains(['=', '\0']))
            {
                return Err(CalmError::BadRequest(
                    "ACP environment cannot override kernel credentials or process markers".into(),
                ));
            }
            if let Some(key) = profile
                .owned_env
                .iter()
                .find(|key| agent.env.contains_key(**key))
            {
                return Err(CalmError::BadRequest(format!(
                    "ACP {} registration cannot set `{key}`: reserved by agent profile; use a native config file",
                    agent.provider.wire_name()
                )));
            }
            if let Some(key) = profile
                .refused_env
                .iter()
                .find(|key| agent.env.contains_key(**key))
            {
                return Err(CalmError::BadRequest(format!(
                    "ACP registration of {} cannot set `{key}`: it signs in with its own CLI, \
                     never an API key",
                    agent.provider.wire_name()
                )));
            }
        }
        Ok(())
    }
}

/// One registered agent and its provider's profile: everything one launch reads.
#[derive(Clone, Copy)]
pub(crate) struct Registration<'a> {
    pub config: &'a AcpAgentConfig,
    pub profile: &'static AcpAgentProfile,
}

#[derive(Debug)]
pub struct AcpHost {
    /// Shared engine binding, reserved for the Planner execution migration.
    pub execution: crate::execution_quiesce::ExecutionLifecycle,
    pub provider_execution: crate::provider_execution::ProviderExecution,
    pub(crate) completion: std::sync::Arc<dyn HostServices>,
    config: Option<AcpPlannerConfig>,
    pub instance: MarkerInstance,
    pub(crate) pre_c4: std::sync::OnceLock<crate::provider_execution::PreC4Sweep>,
    pub mcp_shim: PathBuf,
    pub mcp_socket: PathBuf,
    /// What Neige's MCP server lists each session, for agents whose requests for its tools are
    /// approved by name; without it no tool is approved by name, so a request is the mode's.
    tool_names: Option<crate::mcp_server::transport::SessionToolNames>,
    /// Task callbacks and state are supplied by application composition.
    pub(crate) native_workers: std::sync::Arc<dyn super::roles::NativeWorkers>,
    pub(crate) shared_server: tokio::sync::Mutex<Option<super::server::Server>>,
    pub(crate) context_queries: tokio::sync::Semaphore,
    pub(crate) agent_logs: PathBuf,
    readiness: tokio::sync::Mutex<()>,
    pub(crate) discovery: DiscoveryCache,
    catalogs: std::sync::Mutex<
        std::collections::HashMap<String, (provider::acp::configuration::Configuration, i64)>,
    >,
    pub(crate) _scratch: Option<tempfile::TempDir>,
}
impl AcpHost {
    pub(crate) fn with_services(
        config: Option<AcpPlannerConfig>,
        data_dir: &Path,
        mcp_shim: PathBuf,
        mcp_socket: PathBuf,
        provider_execution: crate::provider_execution::ProviderExecution,
        completion: std::sync::Arc<dyn HostServices>,
        native_workers: std::sync::Arc<dyn super::roles::NativeWorkers>,
    ) -> Result<Self> {
        if let Some(config) = &config {
            config.validate()?;
        }
        std::fs::create_dir_all(data_dir.join("acp-agent-logs"))?;
        Ok(Self {
            provider_execution,
            completion,
            execution: Default::default(),
            pre_c4: std::sync::OnceLock::new(),
            config,
            instance: MarkerInstance::for_data_dir(data_dir, MARKER_KEY)?,
            mcp_shim,
            mcp_socket,
            tool_names: None,
            shared_server: Default::default(),
            native_workers,
            context_queries: tokio::sync::Semaphore::new(1),
            agent_logs: data_dir.join("acp-agent-logs"),
            readiness: tokio::sync::Mutex::new(()),
            discovery: Default::default(),
            catalogs: std::sync::Mutex::new(std::collections::HashMap::new()),
            _scratch: None,
        })
    }
    /// The host reading what Neige's MCP server lists each session from `tool_names`.
    pub fn with_tool_names(
        mut self,
        tool_names: crate::mcp_server::transport::SessionToolNames,
    ) -> Self {
        self.tool_names = Some(tool_names);
        self
    }

    /// The tool names Neige's MCP server lists the worker session `worker` now; none without a
    /// source.
    pub(crate) async fn listed_tools(&self, worker: &str) -> Result<Vec<String>> {
        match &self.tool_names {
            Some(tool_names) => tool_names.listed(worker).await,
            None => Ok(Vec::new()),
        }
    }

    pub fn configured(&self, provider: &AgentProvider) -> Result<&AcpAgentConfig> {
        self.config
            .as_ref()
            .and_then(|config| {
                config
                    .agents
                    .iter()
                    .find(|agent| &agent.provider == provider)
            })
            .ok_or_else(|| {
                CalmError::Conflict(format!(
                    "{} is unavailable: configure --acp-planner-config",
                    provider.wire_name()
                ))
            })
    }

    /// The registration of `provider` with its profile.
    pub(crate) fn registration(&self, provider: &AgentProvider) -> Result<Registration<'_>> {
        let config = self.configured(provider)?;
        let profile = AcpAgentProfile::of(provider).ok_or_else(|| {
            CalmError::Internal(format!("{} has no ACP profile", provider.wire_name()))
        })?;
        Ok(Registration { config, profile })
    }

    pub fn record_configuration(
        &self,
        card: &str,
        configuration: provider::acp::configuration::Configuration,
    ) {
        self.catalogs
            .lock()
            .expect("ACP catalogs")
            .insert(card.to_owned(), (configuration, crate::model::now_ms()));
    }
    pub(crate) fn configuration(
        &self,
        card: &str,
    ) -> Option<(provider::acp::configuration::Configuration, i64)> {
        self.catalogs
            .lock()
            .expect("ACP catalogs")
            .get(card)
            .cloned()
    }

    pub async fn check_ready(&self, provider: &AgentProvider) -> Result<()> {
        let _probe = self.readiness.lock().await;
        let registration = self.registration(provider)?;
        let process =
            super::process::Process::spawn(self, registration, "readiness", &std::env::temp_dir())
                .await?;
        process.stop().await
    }
}

/// Role services are injected by the application. The transport knows no receipt or canonical-input codec.
pub(crate) trait HostServices: std::fmt::Debug + Send + Sync {
    fn repo(&self) -> Result<&std::sync::Arc<dyn crate::db::Repo>>;
    fn bind(&self, repo: std::sync::Arc<dyn crate::db::Repo>);
    fn bind_registry(&self, registry: crate::harness::HarnessRegistry);
    fn request_progress(&self, session: &str);
    fn bind_host(&self, host: std::sync::Weak<AcpHost>);
}
pub(crate) type DiscoveryCache = tokio::sync::Mutex<Option<DiscoveryEntry>>;
#[derive(Debug)]
pub(crate) struct DiscoveryEntry {
    pub provider: AgentProvider,
    pub cwd: Option<PathBuf>,
    pub started: std::time::Instant,
    pub catalog: Option<(provider::acp::configuration::Configuration, i64)>,
}
