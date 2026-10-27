//! Host-owned configuration, pure selection and Codex instance composition.
//! Each supervisor owns its lifecycle; rollout policy keeps product traffic on default.

pub mod codex;
pub mod config;
pub mod identity;
pub mod paths;
pub mod policy;
pub mod signals;

use calm_types::runtime::AgentProvider;
use serde::{Deserialize, Serialize};

/// An operator's stable short name, unique within a provider.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BackendId(String);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("backend id must match [a-z0-9-]{{1,32}}")]
pub struct InvalidBackendId;

impl TryFrom<String> for BackendId {
    type Error = InvalidBackendId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if !(1..=32).contains(&value.len())
            || !value
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(InvalidBackendId);
        }
        Ok(Self(value))
    }
}

impl From<BackendId> for String {
    fn from(value: BackendId) -> Self {
        value.0
    }
}

impl BackendId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_default(&self) -> bool {
        self.0 == "default"
    }
}

/// The native provider and its host configuration scope; a native thread ID alone
/// cannot identify a backend. Not yet a persistent binding in P1.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendRef {
    pub provider: AgentProvider,
    pub id: BackendId,
}

#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod policy_tests;
#[cfg(test)]
mod signals_tests;

#[cfg(test)]
mod identity_tests;
