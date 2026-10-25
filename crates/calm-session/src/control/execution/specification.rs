use super::IoMode;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmbientEnvironment {
    None,
    SupervisorAllowed { proxy_policy: AmbientProxyPolicy },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmbientProxyPolicy {
    RejectSecret,
    Exclude,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecificationVersion {
    ExplicitOnlyV1,
    AmbientV2,
}

#[derive(Clone, Debug, Eq)]
pub struct ExecutionSpecification {
    pub version: SpecificationVersion,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Explicit durable environment; overrides supervisor ambient values at spawn.
    pub environment: Vec<(String, String)>,
    pub ambient_environment: AmbientEnvironment,
    pub io_mode: IoMode,
    pub replay_bytes: usize,
}

// The version records the source format. None has one released representation,
// so its origin marker must not change immutable-input equality after a roundtrip.
impl PartialEq for ExecutionSpecification {
    fn eq(&self, other: &Self) -> bool {
        self.ambient_environment == other.ambient_environment
            && (self.ambient_environment == AmbientEnvironment::None
                || self.version == other.version)
            && self.program == other.program
            && self.args == other.args
            && self.environment == other.environment
            && self.io_mode == other.io_mode
            && self.replay_bytes == other.replay_bytes
    }
}

pub const EXECUTION_SPECIFICATION_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Current {
    version: u32,
    program: PathBuf,
    args: Vec<String>,
    environment: Vec<(String, String)>,
    ambient_environment: AmbientEnvironment,
    io_mode: IoMode,
    replay_bytes: usize,
}

/// Released, unversioned specification: always explicit-only, never inferred ambient.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Legacy {
    program: PathBuf,
    args: Vec<String>,
    environment: Vec<(String, String)>,
    io_mode: IoMode,
    replay_bytes: usize,
}
impl From<Legacy> for ExecutionSpecification {
    fn from(old: Legacy) -> Self {
        Self {
            version: SpecificationVersion::ExplicitOnlyV1,
            program: old.program,
            args: old.args,
            environment: old.environment,
            ambient_environment: AmbientEnvironment::None,
            io_mode: old.io_mode,
            replay_bytes: old.replay_bytes,
        }
    }
}
impl Serialize for ExecutionSpecification {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.version == SpecificationVersion::ExplicitOnlyV1
            && self.ambient_environment != AmbientEnvironment::None
        {
            return Err(serde::ser::Error::custom(
                "legacy specification cannot authorize ambient",
            ));
        }
        if self.ambient_environment == AmbientEnvironment::None {
            return Legacy {
                program: self.program.clone(),
                args: self.args.clone(),
                environment: self.environment.clone(),
                io_mode: self.io_mode.clone(),
                replay_bytes: self.replay_bytes,
            }
            .serialize(serializer);
        }
        if !serializer.is_human_readable() {
            return Err(serde::ser::Error::custom(
                "ambient specification requires PrepareAmbient",
            ));
        }
        Current {
            version: match self.version {
                SpecificationVersion::ExplicitOnlyV1 => 1,
                SpecificationVersion::AmbientV2 => EXECUTION_SPECIFICATION_VERSION,
            },
            program: self.program.clone(),
            args: self.args.clone(),
            environment: self.environment.clone(),
            ambient_environment: self.ambient_environment.clone(),
            io_mode: self.io_mode.clone(),
            replay_bytes: self.replay_bytes,
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for ExecutionSpecification {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let human_readable = deserializer.is_human_readable();
        if !human_readable {
            let old = Legacy::deserialize(deserializer)?;
            return Ok(old.into());
        }
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("version").is_none() {
            let old: Legacy = serde_json::from_value(value).map_err(D::Error::custom)?;
            return Ok(old.into());
        }
        let current = serde_json::from_value::<Current>(value).map_err(D::Error::custom)?;
        if current.version != EXECUTION_SPECIFICATION_VERSION {
            return Err(D::Error::custom(
                "unsupported execution specification version",
            ));
        }
        Ok(Self {
            version: SpecificationVersion::AmbientV2,
            program: current.program,
            args: current.args,
            environment: current.environment,
            ambient_environment: current.ambient_environment,
            io_mode: current.io_mode,
            replay_bytes: current.replay_bytes,
        })
    }
}

/// Only caller-declared public values travel to preflight; the reply never carries values.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvironmentPreflight {
    pub ambient_environment: AmbientEnvironment,
    pub explicit_environment: Vec<(String, String)>,
}
impl From<&ExecutionSpecification> for EnvironmentPreflight {
    fn from(specification: &ExecutionSpecification) -> Self {
        Self {
            ambient_environment: specification.ambient_environment.clone(),
            explicit_environment: specification.environment.clone(),
        }
    }
}
impl EnvironmentPreflight {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.ambient_environment != AmbientEnvironment::None
            && self.explicit_environment.iter().any(|(key, value)| {
                super::proxy::PROXY_KEYS.contains(&key.as_str())
                    && super::proxy::proxy_value_is_secret(key, value.as_ref())
            })
        {
            return Err("secret explicit proxy refused");
        }
        Ok(())
    }
}
impl ExecutionSpecification {
    /// Check durable public values before preparation/admission. None retains the released contract.
    pub fn validate_environment(&self) -> Result<(), &'static str> {
        if self.version == SpecificationVersion::ExplicitOnlyV1
            && self.ambient_environment != AmbientEnvironment::None
        {
            return Err("legacy specification cannot authorize ambient");
        }
        EnvironmentPreflight::from(self).validate()
    }
}

/// The negotiated extension carries a versioned JSON specification. Released binary
/// Prepare keeps its original five-field specification, including its wire ordinals.
pub(super) mod ambient_prepare {
    use super::*;
    use crate::control::execution::PrepareExecution;

    pub fn serialize<S: Serializer>(
        request: &PrepareExecution,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serde_json::to_string(request)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PrepareExecution, D::Error> {
        let json = String::deserialize(deserializer)?;
        serde_json::from_str(&json).map_err(D::Error::custom)
    }
}
