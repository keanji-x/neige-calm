//! Startup JSON and lexical validation only. Filesystem identity belongs to P2.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use calm_types::runtime::{AgentProtocol, AgentProvider};
use serde::Deserialize;

use super::policy::SelectionPolicy;
use super::{BackendId, BackendRef};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendCredentials {
    CodexHome { path: PathBuf },
}

impl BackendCredentials {
    fn protocol(&self) -> AgentProtocol {
        match self {
            Self::CodexHome { .. } => AgentProtocol::Codex,
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::CodexHome { path } => path,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendProfile {
    pub provider: AgentProvider,
    pub id: BackendId,
    pub weight: NonZeroU32,
    /// Only a soft filter for new sessions, never admission or a reservation.
    pub max_inflight: Option<NonZeroU32>,
    pub accepts_new_sessions: bool,
    pub credentials: BackendCredentials,
}

impl BackendProfile {
    pub fn backend_ref(&self) -> BackendRef {
        BackendRef {
            provider: self.provider.clone(),
            id: self.id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBackendConfig {
    #[serde(default)]
    pub policy: SelectionPolicy,
    pub cooldown_after_concurrency_limit_secs: u64,
    pub cooldown_default_secs: u64,
    pub cooldown_max_secs: u64,
    pub backends: Vec<BackendProfile>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading agent backend config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid agent backend JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("duplicate backend: {0:?}")]
    DuplicateBackend(BackendRef),
    #[error("credentials do not implement the backend protocol: {0:?}")]
    CredentialProtocol(BackendRef),
    #[error("backend credentials require an absolute path: {0:?}")]
    RelativePath(BackendRef),
    #[error("cooldowns must be positive and no larger than cooldown_max_secs")]
    InvalidCooldown,
}

impl AgentBackendConfig {
    pub(crate) fn is_multi_backend(&self) -> bool {
        self.backends.len() > 1
    }

    /// Same startup read/parse/validate convention as the planner configurations.
    /// The sole I/O is reading the operator-specified JSON file.
    pub fn read(path: &Path) -> Result<Self, ConfigError> {
        let bytes = std::fs::read(path).map_err(|source| ConfigError::Read {
            path: path.into(),
            source,
        })?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, ConfigError> {
        let config: Self = serde_json::from_slice(bytes)?;
        config.validate()?;
        Ok(config)
    }

    /// No stat, canonicalization, directory creation, permission or overlap claims.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.cooldown_after_concurrency_limit_secs == 0
            || self.cooldown_default_secs == 0
            || self.cooldown_max_secs == 0
            || self.cooldown_after_concurrency_limit_secs > self.cooldown_max_secs
            || self.cooldown_default_secs > self.cooldown_max_secs
        {
            return Err(ConfigError::InvalidCooldown);
        }
        let mut seen = std::collections::HashSet::new();
        for profile in &self.backends {
            let backend = profile.backend_ref();
            if !seen.insert((profile.provider.wire_name(), &profile.id)) {
                return Err(ConfigError::DuplicateBackend(backend));
            }
            if profile.provider.protocol() != profile.credentials.protocol() {
                return Err(ConfigError::CredentialProtocol(backend));
            }
            if !profile.credentials.path().is_absolute() {
                return Err(ConfigError::RelativePath(backend));
            }
        }
        Ok(())
    }

    /// No configuration means the traditional single instance. Its home is only
    /// described here; the existing default owner still initializes it.
    pub fn for_startup(path: Option<&Path>, data_dir: &Path) -> Result<Self, ConfigError> {
        match path {
            Some(path) => Self::read(path),
            None => Ok(Self {
                policy: SelectionPolicy::default(),
                cooldown_after_concurrency_limit_secs: 60,
                cooldown_default_secs: 300,
                cooldown_max_secs: 3600,
                backends: vec![BackendProfile {
                    provider: AgentProvider::Codex,
                    id: BackendId::try_from("default".to_owned()).expect("valid implicit id"),
                    weight: NonZeroU32::MIN,
                    max_inflight: None,
                    accepts_new_sessions: true,
                    credentials: BackendCredentials::CodexHome {
                        path: super::paths::BackendPaths::implicit_home(data_dir),
                    },
                }],
            }),
        }
    }
}
