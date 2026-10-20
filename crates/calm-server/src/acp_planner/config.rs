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
    pub(super) fn native_session_digest(&self, cwd: &Path) -> Result<String> {
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
pub(super) struct Registration<'a> {
    pub config: &'a AcpAgentConfig,
    pub profile: &'static AcpAgentProfile,
}

#[derive(Debug)]
pub struct AcpPlannerHost {
    config: Option<AcpPlannerConfig>,
    pub instance: MarkerInstance,
    pub mcp_shim: PathBuf,
    pub mcp_socket: PathBuf,
    /// What Neige's MCP server lists each session, for agents whose requests for its tools are
    /// approved by name; without it no tool is approved by name, so a request is the mode's.
    tool_names: Option<crate::mcp_server::transport::SessionToolNames>,
    /// Where each agent process's stderr is kept, one bounded file per worker, overwritten by its
    /// next launch.
    pub(super) agent_logs: PathBuf,
    readiness: tokio::sync::Mutex<()>,
    pub(super) discovery: super::catalog::DiscoveryCache,
    catalogs: std::sync::Mutex<
        std::collections::HashMap<String, (provider::acp::configuration::Configuration, i64)>,
    >,
    _scratch: Option<tempfile::TempDir>,
}
impl AcpPlannerHost {
    pub fn new(
        config: Option<AcpPlannerConfig>,
        data_dir: &Path,
        mcp_shim: PathBuf,
        mcp_socket: PathBuf,
    ) -> Result<Self> {
        if let Some(config) = &config {
            config.validate()?;
        }
        Ok(Self {
            config,
            instance: MarkerInstance::for_data_dir(data_dir, MARKER_KEY)?,
            mcp_shim,
            mcp_socket,
            tool_names: None,
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
    pub(super) async fn listed_tools(&self, worker: &str) -> Result<Vec<String>> {
        match &self.tool_names {
            Some(tool_names) => tool_names.listed(worker).await,
            None => Ok(Vec::new()),
        }
    }

    pub fn unconfigured_scratch() -> Result<Self> {
        let scratch = tempfile::Builder::new().prefix("neige-acp-").tempdir()?;
        let mut host = Self::new(
            None,
            scratch.path(),
            "neige-mcp-stdio-shim".into(),
            scratch.path().join("mcp.sock"),
        )?;
        host._scratch = Some(scratch);
        Ok(host)
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
    pub(super) fn registration(&self, provider: &AgentProvider) -> Result<Registration<'_>> {
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
    pub(super) fn configuration(
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
        let process = super::process::Process::spawn(
            self,
            registration,
            "readiness",
            &std::env::temp_dir(),
            super::process::LaunchContext::Readiness,
        )
        .await?;
        process.stop(self, "readiness").await
    }
}

/// Revoke owned credentials before listening, even without a current registration. A failed
/// sweep only warns here; every launch still requires its own successful stop before spawning.
pub async fn boot(repo: &dyn crate::db::RepoEventWrite, host: &AcpPlannerHost) -> Result<()> {
    let ids = revoke_owned(repo, None).await?;
    let mut ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    ids.push(super::catalog::WORKER);
    if let Err(error) = crate::planner_process::sweep(
        &host.instance,
        &ids,
        crate::planner_process::SeamPolicy::Ignore,
    )
    .await
    {
        tracing::warn!(
            %error,
            "ACP boot cleanup failed; old MCP credentials revoked; boot continues; each launch still requires a successful stop"
        );
    }
    Ok(())
}

pub async fn sweep_track(
    repo: &dyn crate::db::RepoEventWrite,
    host: &AcpPlannerHost,
    track: &str,
) -> Result<()> {
    let ids = revoke_owned(repo, Some(track.to_owned())).await?;
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    crate::planner_process::sweep(
        &host.instance,
        &ids,
        crate::planner_process::SeamPolicy::Consult,
    )
    .await
}

async fn revoke_owned(
    repo: &dyn crate::db::RepoEventWrite,
    track: Option<String>,
) -> Result<Vec<String>> {
    crate::db::write_in_tx_typed(repo, move |tx| Box::pin(async move {
        let ids = sqlx::query_scalar(concat!("SELECT ws.id FROM acp_managed_sessions owned JOIN worker_sessions ws ON ","ws.id=owned.worker_session_id WHERE (?1 IS NULL OR ws.track_id=?1)")).bind(&track).fetch_all(&mut **tx).await?;
        sqlx::query(concat!("UPDATE worker_sessions SET mcp_token_hash=NULL WHERE id IN (SELECT worker_session_id FROM ","acp_managed_sessions) AND (?1 IS NULL OR track_id=?1)")).bind(track).execute(&mut **tx).await?;
        Ok(ids)
    })).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(agent: serde_json::Value) -> Result<AcpPlannerConfig> {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), json!({"agents":[agent]}).to_string()).unwrap();
        AcpPlannerConfig::read(file.path())
    }
    fn opencode() -> serde_json::Value {
        json!({"provider":"opencode","command":"/opt/opencode","args":[],"env":{},
            "min_agent_version":"1.18.35"})
    }

    #[test]
    fn creation_permission_mode_is_optional_and_strict() {
        assert_eq!(
            read(opencode()).unwrap().agents[0].default_permission_mode,
            None
        );
        for mode in ["never", "ask", "full"] {
            let mut agent = opencode();
            agent["default_permission_mode"] = json!(mode);
            assert_eq!(
                serde_json::to_value(
                    read(agent).unwrap().agents[0]
                        .default_permission_mode
                        .unwrap()
                )
                .unwrap(),
                json!(mode)
            );
        }
        for invalid in [
            json!("Full"),
            json!("unknown"),
            json!(null),
            json!(true),
            json!(1),
            json!({"full":null}),
            json!([]),
        ] {
            let mut agent = opencode();
            agent["default_permission_mode"] = invalid.clone();
            assert!(read(agent).is_err(), "{invalid}");
        }
    }

    #[test]
    fn a_registration_reads_its_minimum_version_as_semver() {
        let config = read(opencode()).unwrap();
        assert_eq!(
            config.agents[0].min_agent_version,
            semver::Version::new(1, 18, 35)
        );
        for version in ["1.18", "v1.18.35", "latest", ""] {
            let mut agent = opencode();
            agent["min_agent_version"] = json!(version);
            assert!(read(agent).is_err(), "{version}");
        }
        let mut agent = opencode();
        agent.as_object_mut().unwrap().remove("min_agent_version");
        assert!(read(agent).is_err(), "min_agent_version is required");
        // The ACP entry is the profile's; a registration cannot name it.
        let mut agent = opencode();
        agent["subcommand"] = json!(["acp"]);
        assert!(read(agent).is_err(), "subcommand is unknown");
    }

    /// An older file names what replaced its fields, beside the parse error and its position, and
    /// the old and new forms are never accepted together.
    #[test]
    fn replaced_identity_fields_are_refused_with_their_replacement() {
        for (field, value, hint) in [
            (
                "expected_agent_version",
                "1.18.35",
                "rename it to `min_agent_version`",
            ),
            (
                "expected_agent_name",
                "OpenCode",
                "the provider's profile fixes",
            ),
        ] {
            // Alone, as an older file has it with its `acp` argument, and beside its replacement.
            for keep_new_form in [false, true] {
                let mut agent = opencode();
                if !keep_new_form {
                    agent.as_object_mut().unwrap().remove("min_agent_version");
                    agent["args"] = json!(["acp"]);
                }
                agent[field] = json!(value);
                let message = read(agent).unwrap_err().to_string();
                assert!(
                    message.contains(field)
                        && message.contains(hint)
                        && message.contains("`args` no longer holds the ACP subcommand")
                        && message.contains("at line 1 column"),
                    "{message}"
                );
            }
        }
    }

    /// A file as older versions wrote it is told every change at once.
    #[test]
    fn an_older_file_is_told_every_replaced_field() {
        let mut agent = opencode();
        let object = agent.as_object_mut().unwrap();
        object.remove("min_agent_version");
        object.insert("args".into(), json!(["acp"]));
        object.insert("expected_agent_name".into(), json!("OpenCode"));
        object.insert("expected_agent_version".into(), json!("1.18.34"));
        let message = read(agent).unwrap_err().to_string();
        assert!(
            message.contains("`expected_agent_name` was replaced; remove it")
                && message.contains("`expected_agent_version` was replaced; rename it"),
            "{message}"
        );
    }

    /// A parse error of a current file keeps its position.
    #[test]
    fn a_parse_error_names_its_position() {
        let mut agent = opencode();
        agent["min_agent_version"] = json!("latest");
        let message = read(agent).unwrap_err().to_string();
        assert!(message.contains("at line 1 column"), "{message}");
    }

    /// A registration may not hand Grok an xAI API key: Neige runs it on its signed-in
    /// subscription only. Every other provider keeps its environment as registered.
    #[test]
    fn a_grok_registration_refuses_xai_api_keys() {
        let grok = |env: serde_json::Value| {
            json!({"provider":"grok","command":"/opt/grok/grok-1.0.62","args":[],"env":env,
                "min_agent_version":"1.0.62"})
        };
        assert!(read(grok(json!({"GROK_HOME":"/var/lib/neige/grok"}))).is_ok());
        for key in ["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"] {
            let message = read(grok(json!({ key: "xai-secret" })))
                .unwrap_err()
                .to_string();
            assert!(
                message.contains(&format!("cannot set `{key}`")) && !message.contains("xai-secret"),
                "{message}"
            );
        }
        let mut agent = opencode();
        agent["env"] = json!({"XAI_API_KEY":"unrelated"});
        assert!(read(agent).is_ok());
    }

    /// Only ACP providers register, each at most once.
    #[test]
    fn a_registration_names_each_acp_provider_at_most_once() {
        let grok = json!({"provider":"grok","command":"/opt/grok","args":[],"env":{},
            "min_agent_version":"1.0.62"});
        let file = tempfile::NamedTempFile::new().unwrap();
        let write = |agents: serde_json::Value| {
            std::fs::write(file.path(), json!({ "agents": agents }).to_string()).unwrap();
            AcpPlannerConfig::read(file.path())
        };
        assert!(write(json!([opencode(), grok.clone()])).is_ok());
        assert!(write(json!([grok.clone(), grok])).is_err());
        let mut codex = opencode();
        codex["provider"] = json!("codex");
        assert!(write(json!([codex])).is_err());
    }

    /// The native session key covers the agent process and its directory, not admission policy.
    #[test]
    fn only_the_agent_process_and_cwd_key_a_native_session() {
        let digest = |agent: serde_json::Value, cwd: &str| {
            read(agent).unwrap().agents[0]
                .native_session_digest(Path::new(cwd))
                .unwrap()
        };
        let base = digest(opencode(), "/work");
        for mode in ["never", "ask", "full"] {
            let mut agent = opencode();
            agent["default_permission_mode"] = json!(mode);
            assert_eq!(
                digest(agent, "/work"),
                base,
                "default_permission_mode={mode}"
            );
        }
        let mut raised = opencode();
        raised["min_agent_version"] = json!("1.19.0");
        assert_eq!(digest(raised, "/work"), base, "min_agent_version");
        for (field, value) in [
            ("command", json!("/opt/other/opencode")),
            ("args", json!(["--wrapped"])),
            ("env", json!({"HOME":"/private/other"})),
        ] {
            let mut changed = opencode();
            changed[field] = value;
            assert_ne!(digest(changed, "/work"), base, "{field}");
        }
        assert_ne!(digest(opencode(), "/elsewhere"), base, "cwd");
    }
}
