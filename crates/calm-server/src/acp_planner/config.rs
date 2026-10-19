use super::profile::AcpAgentProfile;
use crate::error::{CalmError, Result};
use crate::planner_process::MarkerInstance;
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
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcpAgentConfig {
    pub provider: AgentProvider,
    pub command: PathBuf,
    /// The arguments before the agent options a Planner turn's permission mode adds.
    pub args: Vec<String>,
    /// The arguments after those agent options: the ACP subcommand and its own arguments.
    pub subcommand: Vec<String>,
    /// Complete operator-declared environment additions. Nothing is inherited implicitly.
    pub env: BTreeMap<String, String>,
    /// The lowest agent version a launch accepts; the agent's profile says where it reports it.
    #[serde(with = "semver_text")]
    pub min_agent_version: semver::Version,
}

/// A semver version as its text, as the registration writes it and its digest records it.
mod semver_text {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(version: &semver::Version, out: S) -> Result<S::Ok, S::Error> {
        out.collect_str(version)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(input: D) -> Result<semver::Version, D::Error> {
        let text = String::deserialize(input)?;
        semver::Version::parse(&text)
            .map_err(|error| D::Error::custom(format!("`min_agent_version` {text:?}: {error}")))
    }
}

/// Registration fields this version no longer reads, and what replaced each. Named apart from
/// the generic unknown-field refusal so an operator's older file says what to change.
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

impl AcpPlannerConfig {
    pub fn read(path: &Path) -> Result<Self> {
        let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        if let Some(agents) = raw.get("agents").and_then(serde_json::Value::as_array) {
            for (field, change) in REPLACED_FIELDS {
                if agents.iter().any(|agent| agent.get(field).is_some()) {
                    return Err(CalmError::BadRequest(format!(
                        "ACP registration field `{field}` was replaced; {change}"
                    )));
                }
            }
        }
        let config: Self = serde_json::from_value(raw)?;
        config.validate()?;
        Ok(config)
    }
    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for agent in &self.agents {
            if agent.provider.protocol() != AgentProtocol::Acp
                || !seen.insert(agent.provider.wire_name())
            {
                return Err(CalmError::BadRequest(
                    "ACP registration names an unsupported or duplicate agent".into(),
                ));
            }
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
    /// [`AcpAgentProfile::of`]; a unit test substitutes a profile no provider registers yet.
    profiles: fn(&AgentProvider) -> Option<&'static AcpAgentProfile>,
    pub instance: MarkerInstance,
    pub mcp_shim: PathBuf,
    pub mcp_socket: PathBuf,
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
            profiles: AcpAgentProfile::of,
            instance: MarkerInstance::for_data_dir(data_dir, MARKER_KEY)?,
            mcp_shim,
            mcp_socket,
            readiness: tokio::sync::Mutex::new(()),
            discovery: Default::default(),
            catalogs: std::sync::Mutex::new(std::collections::HashMap::new()),
            _scratch: None,
        })
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
        let profile = (self.profiles)(provider).ok_or_else(|| {
            CalmError::Internal(format!("{} has no ACP profile", provider.wire_name()))
        })?;
        Ok(Registration { config, profile })
    }
    #[cfg(test)]
    pub(super) fn with_profiles(
        mut self,
        profiles: fn(&AgentProvider) -> Option<&'static AcpAgentProfile>,
    ) -> Self {
        self.profiles = profiles;
        self
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
        json!({"provider":"opencode","command":"/opt/opencode","args":[],"subcommand":["acp"],
            "env":{},"min_agent_version":"1.18.35"})
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
        for field in ["min_agent_version", "subcommand"] {
            let mut agent = opencode();
            agent.as_object_mut().unwrap().remove(field);
            assert!(read(agent).is_err(), "{field} is required");
        }
    }

    /// An older file names what replaced its fields instead of failing as an unknown key, and the
    /// old and new forms are never accepted together.
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
            // Alone, as an older file has it, and beside its replacement.
            for keep_new_form in [false, true] {
                let mut agent = opencode();
                if !keep_new_form {
                    agent.as_object_mut().unwrap().remove("min_agent_version");
                }
                agent[field] = json!(value);
                let message = read(agent).unwrap_err().to_string();
                assert!(
                    message.contains(field) && message.contains(hint),
                    "{message}"
                );
            }
        }
    }

    /// The digest of a registration records its minimum version as written.
    #[test]
    fn a_registration_serializes_its_minimum_version_as_text() {
        let config = read(opencode()).unwrap();
        assert_eq!(
            serde_json::to_value(&config.agents[0]).unwrap()["min_agent_version"],
            "1.18.35"
        );
    }
}
