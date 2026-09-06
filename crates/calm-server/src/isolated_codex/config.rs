//! Explicit kernel configuration; no provider credentials enter a Worker shell.
use crate::dedicated_codex::{Controller, ControllerConfig, HomeSeed};
use crate::error::{CalmError, Result};
use calm_worker_runtime::RuntimeConfig;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolatedCodexConfig {
    pub workspace_root: PathBuf,
    pub private_root: PathBuf,
    pub runtime_root: PathBuf,
    pub runtime_helper: PathBuf,
    pub runtime_bwrap: PathBuf,
    pub sandbox_bwrap: PathBuf,
    pub codex_binary: PathBuf,
    pub code_mode_host_binary: PathBuf,
    pub mcp_shim: PathBuf,
    pub provider_config: PathBuf,
    pub provider_auth: PathBuf,
    pub provider_environment: BTreeMap<String, String>,
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub task_timeout_ms: u64,
}

pub struct Backend {
    pub(crate) controller: Arc<Controller>,
    pub(crate) seed: HomeSeed,
    pub(crate) workspace_root: PathBuf,
    pub(crate) task_timeout_ms: i64,
    pub(crate) observers: Arc<Mutex<HashSet<String>>>,
}
impl Backend {
    pub fn new(config: IsolatedCodexConfig) -> Result<Self> {
        Self::build(config, None)
    }
    #[cfg(feature = "fixtures")]
    pub fn with_fixture_arguments(config: IsolatedCodexConfig, args: Vec<String>) -> Result<Self> {
        Self::build(config, Some(args))
    }
    fn build(config: IsolatedCodexConfig, fixture_arguments: Option<Vec<String>>) -> Result<Self> {
        if config.task_timeout_ms == 0 || config.task_timeout_ms > 86_400_000 {
            return Err(CalmError::BadRequest(
                "isolated task timeout must be within one day".into(),
            ));
        }
        super::workspace::prepare_root(&config.workspace_root)?;
        let workspace_root = config.workspace_root.canonicalize()?;
        for private in [&config.private_root, &config.runtime_root] {
            if private.starts_with(&workspace_root) || workspace_root.starts_with(private) {
                return Err(CalmError::BadRequest(
                    "isolated private roots must be outside the workspace root".into(),
                ));
            }
        }
        let seed = HomeSeed::read(&config.provider_config, &config.provider_auth)
            .map_err(provider_error)?;
        let controller = Controller::new(ControllerConfig {
            private_root: config.private_root,
            runtime: RuntimeConfig {
                state_root: config.runtime_root,
                helper: config.runtime_helper,
                bwrap: config.runtime_bwrap,
                timeout: Duration::from_secs(5),
            },
            sandbox_bwrap: config.sandbox_bwrap,
            codex_binary: config.codex_binary,
            code_mode_host_binary: config.code_mode_host_binary,
            mcp_shim: config.mcp_shim,
            provider_environment: config.provider_environment,
            connect_timeout: Duration::from_millis(config.connect_timeout_ms),
            request_timeout: Duration::from_millis(config.request_timeout_ms),
        })
        .map_err(provider_error)?;
        #[cfg(feature = "fixtures")]
        let controller = match fixture_arguments {
            Some(args) => controller.with_fixture_arguments(args),
            None => controller,
        };
        #[cfg(not(feature = "fixtures"))]
        if fixture_arguments.is_some() {
            return Err(CalmError::Internal("fixture arguments unavailable".into()));
        }
        Ok(Self {
            controller: Arc::new(controller),
            seed,
            workspace_root,
            task_timeout_ms: config.task_timeout_ms as i64,
            observers: Arc::new(Mutex::new(HashSet::new())),
        })
    }
}
pub(crate) fn provider_error(error: crate::dedicated_codex::Error) -> CalmError {
    CalmError::Conflict(error.to_string())
}
