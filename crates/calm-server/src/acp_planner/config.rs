use crate::error::{CalmError, Result};
use crate::planner_permission_mode::PlannerPermissionMode;
use crate::planner_process::MarkerInstance;
use calm_types::runtime::AgentProvider;
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
    pub args: Vec<String>,
    /// Complete operator-declared environment additions. Nothing is inherited implicitly.
    pub env: BTreeMap<String, String>,
    pub expected_agent_name: String,
    pub expected_agent_version: String,
}
/// What OpenCode's own `permission` configuration is overridden with under `ask` (#2348): the
/// tools that act without asking under its defaults ask instead. OpenCode 1.18.35 merges this
/// variable over every config file, and has no flag or ACP method for it. `edit` covers its
/// write and patch tools too. Paths outside the workspace do not ask on their own (#2452): an
/// outside read runs, as it does for Codex and Claude, and an outside command or edit asks once,
/// through its tool, instead of once for the directory and again for the tool.
const OPENCODE_ASK_PERMISSION: (&str, &str) = (
    "OPENCODE_PERMISSION",
    r#"{"bash":"ask","edit":"ask","webfetch":"ask","external_directory":"allow"}"#,
);

/// What OpenCode's `permission` configuration is overridden with under `full` (#2441): every
/// tool that acts, reads or fetches is allowed without asking, outside the workspace too. No `*`,
/// so OpenCode keeps denying the tools a Planner has no use for (`question`, `plan_*`).
const OPENCODE_FULL_PERMISSION: (&str, &str) = (
    "OPENCODE_PERMISSION",
    r#"{"bash":"allow","edit":"allow","webfetch":"allow","external_directory":"allow","doom_loop":"allow","read":"allow"}"#,
);

impl AcpPlannerConfig {
    pub fn read(path: &Path) -> Result<Self> {
        let config: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        config.validate()?;
        Ok(config)
    }
    fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for agent in &self.agents {
            if agent.provider != AgentProvider::OpenCode || !seen.insert(agent.provider.wire_name())
            {
                return Err(CalmError::BadRequest(
                    "ACP registration names an unsupported or duplicate agent".into(),
                ));
            }
            if !agent.command.is_absolute()
                || agent.expected_agent_name.is_empty()
                || agent.expected_agent_version.is_empty()
            {
                return Err(CalmError::BadRequest("ACP registration requires an absolute command and pinned agent identity/version".into()));
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
impl AcpAgentConfig {
    /// The environment one Planner turn's process adds under `mode`, set explicitly, never
    /// inherited. `never` adds nothing: the agent's own configuration stands, as before.
    pub fn permission_env(&self, mode: PlannerPermissionMode) -> Vec<(&'static str, &'static str)> {
        match (mode, &self.provider) {
            (PlannerPermissionMode::Never, _) => Vec::new(),
            (PlannerPermissionMode::Ask, AgentProvider::OpenCode) => vec![OPENCODE_ASK_PERMISSION],
            (PlannerPermissionMode::Full, AgentProvider::OpenCode) => {
                vec![OPENCODE_FULL_PERMISSION]
            }
            // Not ACP agents: a registration naming them is refused.
            (
                PlannerPermissionMode::Ask | PlannerPermissionMode::Full,
                AgentProvider::Codex | AgentProvider::Claude,
            ) => Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct AcpPlannerHost {
    config: Option<AcpPlannerConfig>,
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
    pub fn configuration(
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
        let config = self.configured(provider)?;
        let process = super::process::Process::spawn(
            self,
            config,
            "readiness",
            &std::env::temp_dir(),
            super::process::LaunchContext::Readiness,
        )
        .await?;
        process.stop(self, "readiness").await
    }
}

/// Only sessions explicitly registered by this ACP backend are cleanup candidates.
pub async fn boot(repo: &dyn crate::db::RepoEventWrite, host: &AcpPlannerHost) -> Result<()> {
    let ids = revoke_owned(repo, None).await?;
    let mut ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    ids.push(super::catalog::WORKER);
    crate::planner_process::sweep(
        &host.instance,
        &ids,
        crate::planner_process::SeamPolicy::Ignore,
    )
    .await
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

    fn opencode() -> AcpAgentConfig {
        AcpAgentConfig {
            provider: AgentProvider::OpenCode,
            command: "/opt/opencode".into(),
            args: vec!["acp".into()],
            env: BTreeMap::new(),
            expected_agent_name: "OpenCode".into(),
            expected_agent_version: "1.18.35".into(),
        }
    }

    /// `ask` makes OpenCode ask before bash, edits and fetches, and only through those tools: a
    /// path outside the workspace asks nothing more (#2452). Never with a `*` rule. `never` adds
    /// nothing, so its launch is today's.
    #[test]
    fn only_ask_overrides_opencodes_permission_config() {
        assert_eq!(
            opencode().permission_env(PlannerPermissionMode::Ask),
            vec![(
                "OPENCODE_PERMISSION",
                r#"{"bash":"ask","edit":"ask","webfetch":"ask","external_directory":"allow"}"#
            )]
        );
        assert_eq!(
            opencode().permission_env(PlannerPermissionMode::Never),
            Vec::<(&str, &str)>::new()
        );
    }

    /// #2441: `full` allows each acting tool by name, outside the workspace too, and never with a
    /// `*` rule, which would also allow the tools OpenCode denies a Planner.
    #[test]
    fn full_allows_each_acting_tool_by_name() {
        let env = opencode().permission_env(PlannerPermissionMode::Full);
        let [(key, value)] = env.as_slice() else {
            panic!("one override: {env:?}");
        };
        assert_eq!(*key, "OPENCODE_PERMISSION");
        let permission: serde_json::Value = serde_json::from_str(value).unwrap();
        assert_eq!(
            permission,
            serde_json::json!({
                "bash": "allow",
                "edit": "allow",
                "webfetch": "allow",
                "external_directory": "allow",
                "doom_loop": "allow",
                "read": "allow",
            })
        );
        assert!(permission.get("*").is_none(), "{permission}");
    }
}
