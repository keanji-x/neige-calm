//! Internal shared-daemon recovery protocol. Scope identifiers are minted by the
//! process owner; the database enforces publication and retirement boundaries.
use crate::error::{Result, TruthError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryId(uuid::Uuid);

impl RecoveryId {
    pub fn fresh() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub(crate) fn decode(value: &str) -> Result<Self> {
        let id = uuid::Uuid::parse_str(value)
            .map_err(|_| TruthError::Internal("invalid daemon recovery id".into()))?;
        if id.is_nil() || id.to_string() != value {
            return Err(TruthError::Internal(
                "noncanonical daemon recovery id".into(),
            ));
        }
        Ok(Self(id))
    }
}

impl std::fmt::Display for RecoveryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonRecovery {
    LegacyUntracked,
    Clear,
    Pending { id: RecoveryId },
    Recorded { id: RecoveryId },
}

impl DaemonRecovery {
    pub fn scope_id(self) -> Option<RecoveryId> {
        match self {
            Self::LegacyUntracked | Self::Clear => None,
            Self::Pending { id } | Self::Recorded { id } => Some(id),
        }
    }

    pub(crate) fn encode(self) -> (&'static str, Option<String>) {
        match self {
            Self::LegacyUntracked => ("legacy", None),
            Self::Clear => ("clear", None),
            Self::Pending { id } => ("pending", Some(id.to_string())),
            Self::Recorded { id } => ("recorded", Some(id.to_string())),
        }
    }

    pub(crate) fn decode(phase: &str, id: Option<&str>) -> Result<Self> {
        match (phase, id) {
            ("legacy", None) => Ok(Self::LegacyUntracked),
            ("clear", None) => Ok(Self::Clear),
            ("pending", Some(id)) => Ok(Self::Pending {
                id: RecoveryId::decode(id)?,
            }),
            ("recorded", Some(id)) => Ok(Self::Recorded {
                id: RecoveryId::decode(id)?,
            }),
            _ => Err(TruthError::Internal(
                "invalid daemon recovery phase/id pair".into(),
            )),
        }
    }
}

/// No operation can silently replace a committed obligation. `Legacy` is only
/// writable while the row is untracked; it cannot erase Pending or Recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryWrite {
    Legacy,
    Begin { id: RecoveryId },
    ClaimLegacy { id: RecoveryId },
    Publish { id: RecoveryId },
    Preserve { expected: DaemonRecovery },
    CancelReserved { id: RecoveryId },
    Settle { id: RecoveryId },
}
