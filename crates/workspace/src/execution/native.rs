//! Native identities are audit facts; only a continuously owned handle authorizes control.
use super::CapabilityToken;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxProcessIdentity {
    pub pid: u32,
    pub start_time_ticks: u64,
    pub nonce: CapabilityToken,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacProcessIdentity {
    pub pid: u32,
    pub birth_seconds: u64,
    pub birth_microseconds: u64,
    pub nonce: CapabilityToken,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeProcessIdentity {
    LinuxHelper(LinuxProcessIdentity),
    MacOwnedGroup(MacProcessIdentity),
}
impl NativeProcessIdentity {
    pub fn pid(&self) -> u32 {
        match self {
            Self::LinuxHelper(p) => p.pid,
            Self::MacOwnedGroup(p) => p.pid,
        }
    }
    pub fn nonce(&self) -> &CapabilityToken {
        match self {
            Self::LinuxHelper(p) => &p.nonce,
            Self::MacOwnedGroup(p) => &p.nonce,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxHelperIdentity {
    pub executable_device: u64,
    pub executable_inode: u64,
    pub protocol_version: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeMechanismIdentity {
    LinuxHelper(LinuxHelperIdentity),
    MacOwnedGroup { protocol_version: u32 },
}

// Keep released Linux JSON records byte-shape compatible without defaulting any fact.
// Bincode uses the new negotiated envelope and explicit platform ordinals.
#[derive(Serialize, Deserialize)]
enum ProcessWire {
    LinuxHelper(LinuxProcessIdentity),
    MacOwnedGroup(MacProcessIdentity),
}
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum ProcessJson {
    Legacy(LinuxProcessIdentity),
    Native(ProcessWire),
}
#[derive(Serialize, Deserialize)]
enum MechanismWire {
    LinuxHelper(LinuxHelperIdentity),
    MacOwnedGroup { protocol_version: u32 },
}
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum MechanismJson {
    Legacy(LinuxHelperIdentity),
    Native(MechanismWire),
}
impl Serialize for NativeProcessIdentity {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let wire = match self {
            Self::LinuxHelper(p) => ProcessWire::LinuxHelper(p.clone()),
            Self::MacOwnedGroup(p) => ProcessWire::MacOwnedGroup(p.clone()),
        };
        if s.is_human_readable() {
            match wire {
                ProcessWire::LinuxHelper(p) => ProcessJson::Legacy(p),
                other => ProcessJson::Native(other),
            }
            .serialize(s)
        } else {
            wire.serialize(s)
        }
    }
}
impl<'de> Deserialize<'de> for NativeProcessIdentity {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let wire = if d.is_human_readable() {
            match ProcessJson::deserialize(d)? {
                ProcessJson::Legacy(p) => ProcessWire::LinuxHelper(p),
                ProcessJson::Native(p) => p,
            }
        } else {
            ProcessWire::deserialize(d)?
        };
        Ok(match wire {
            ProcessWire::LinuxHelper(p) => Self::LinuxHelper(p),
            ProcessWire::MacOwnedGroup(p) => Self::MacOwnedGroup(p),
        })
    }
}
impl Serialize for ScopeMechanismIdentity {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let wire = match self {
            Self::LinuxHelper(p) => MechanismWire::LinuxHelper(p.clone()),
            Self::MacOwnedGroup { protocol_version } => MechanismWire::MacOwnedGroup {
                protocol_version: *protocol_version,
            },
        };
        if s.is_human_readable() {
            match wire {
                MechanismWire::LinuxHelper(p) => MechanismJson::Legacy(p),
                other => MechanismJson::Native(other),
            }
            .serialize(s)
        } else {
            wire.serialize(s)
        }
    }
}
impl<'de> Deserialize<'de> for ScopeMechanismIdentity {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let wire = if d.is_human_readable() {
            match MechanismJson::deserialize(d)? {
                MechanismJson::Legacy(p) => MechanismWire::LinuxHelper(p),
                MechanismJson::Native(p) => p,
            }
        } else {
            MechanismWire::deserialize(d)?
        };
        Ok(match wire {
            MechanismWire::LinuxHelper(p) => Self::LinuxHelper(p),
            MechanismWire::MacOwnedGroup { protocol_version } => {
                Self::MacOwnedGroup { protocol_version }
            }
        })
    }
}

/// Restart observation only; excludes descendants which escaped with setsid.
/// This is deliberately distinct from the helper's descendant drain proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptedQuiescenceProof {
    pub host_boot_id: String,
    pub helper_process: NativeProcessIdentity,
    pub mechanism: ScopeMechanismIdentity,
    pub scope: InterruptedScope,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterruptedScope {
    LinuxOriginalGroupAndSessionEmpty,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionInterruption {
    ExecutorInterrupted,
}
