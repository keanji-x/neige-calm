//! Internal preparation primitive. No HTTP, tool, or model entry point creates claims.
//! A future durable handoff operation must journal its request before sending it.
use serde::{Deserialize, Serialize};

use crate::ids::{CardId, TrackId};

/// Neige-owned claim and exact session binding, carried unchanged on retries and compensation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PauseRequest {
    claim_id: String,
    card_id: CardId,
    session_id: String,
    track_id: TrackId,
}

impl PauseRequest {
    /// Allocate once in the operation owner, then durably retain before submitting.
    pub fn new(card_id: CardId, session_id: String, track_id: TrackId) -> Self {
        Self {
            claim_id: crate::model::new_id(),
            card_id,
            session_id,
            track_id,
        }
    }

    pub fn matches_binding(&self, card: &CardId, session: &str, track: &TrackId) -> bool {
        self.card_id == *card && self.session_id == session && self.track_id == *track
    }

    pub(crate) fn identity(&self) -> (&CardId, &str, &TrackId) {
        (&self.card_id, &self.session_id, &self.track_id)
    }

    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        if uuid::Uuid::parse_str(&self.claim_id).is_err()
            || self.session_id.is_empty()
            || self.card_id.as_str().is_empty()
            || self.track_id.as_str().is_empty()
        {
            return Err("invalid handoff pause identity");
        }
        Ok(())
    }
}

/// A receipt is the persisted request, never a process-local lease. There is no TTL or Drop unlock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PauseReceipt(PauseRequest);

impl PauseReceipt {
    pub(crate) fn from_request(request: PauseRequest) -> Self {
        Self(request)
    }

    pub fn request(&self) -> &PauseRequest {
        &self.0
    }

    pub fn matches_binding(&self, card: &CardId, session: &str, track: &TrackId) -> bool {
        self.0.matches_binding(card, session, track)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PauseState {
    #[default]
    Unpaused,
    Paused {
        receipt: PauseReceipt,
    },
    FaultedPaused {
        receipt: PauseReceipt,
        fault: BackendFault,
    },
    RecoveryHealthUnconfirmed {
        receipt: PauseReceipt,
    },
    ConsumedByHandoff {
        receipt: PauseReceipt,
        plan_id: String,
        operation_id: String,
        new_card_id: CardId,
        new_session_id: String,
    },
    Released {
        receipt: PauseReceipt,
        outcome: ReleaseOutcome,
    },
}

/// A provider failure is independent of the maintenance claim, never an availability guess.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendFault {
    ThreadSystemError,
}

impl BackendFault {
    pub(crate) fn released_state(&self) -> super::HarnessState {
        match self {
            Self::ThreadSystemError => super::HarnessState::Wedged {
                since: std::time::Instant::now(),
                reason: calm_types::harness::HARNESS_SYSTEM_ERROR_REASON.into(),
            },
        }
    }
}

/// Releasing a maintenance claim does not establish provider readiness.
/// Future handoff operations must abandon preparation on either nonhealthy outcome.
#[must_use = "a fault or unconfirmed health must not be treated as healthy compensation"]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReleaseOutcome {
    HealthyReleased,
    HealthUnconfirmedAfterRecovery,
    ReleasedWithBackendFault { fault: BackendFault },
}

// Only an absent field defaults. Null, malformed identities and unknown variants fail recovery.
impl<'de> Deserialize<'de> for PauseState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
        enum Stored {
            Unpaused,
            RecoveryHealthUnconfirmed {
                receipt: PauseReceipt,
            },
            ConsumedByHandoff {
                receipt: PauseReceipt,
                plan_id: String,
                operation_id: String,
                new_card_id: CardId,
                new_session_id: String,
            },
            Released {
                receipt: PauseReceipt,
                outcome: ReleaseOutcome,
            },
            // Explicit legacy healthy representation: absence of a fault is valid only here.
            Paused {
                receipt: PauseReceipt,
            },
            FaultedPaused {
                receipt: PauseReceipt,
                fault: BackendFault,
            },
        }
        let state = match Stored::deserialize(deserializer)? {
            Stored::ConsumedByHandoff {
                receipt,
                plan_id,
                operation_id,
                new_card_id,
                new_session_id,
            } => {
                if [
                    &plan_id,
                    &operation_id,
                    new_card_id.as_str(),
                    &new_session_id,
                ]
                .iter()
                .any(|s| s.is_empty())
                {
                    return Err(serde::de::Error::custom("invalid handoff disposition"));
                }
                Self::ConsumedByHandoff {
                    receipt,
                    plan_id,
                    operation_id,
                    new_card_id,
                    new_session_id,
                }
            }
            Stored::Unpaused => Self::Unpaused,
            Stored::RecoveryHealthUnconfirmed { receipt } => {
                Self::RecoveryHealthUnconfirmed { receipt }
            }
            Stored::Released { receipt, outcome } => Self::Released { receipt, outcome },
            Stored::Paused { receipt } => Self::Paused { receipt },
            Stored::FaultedPaused { receipt, fault } => Self::FaultedPaused { receipt, fault },
        };
        if let Some(receipt) = state.receipt() {
            receipt
                .request()
                .validate()
                .map_err(serde::de::Error::custom)?;
        }
        Ok(state)
    }
}

impl PauseState {
    pub fn receipt(&self) -> Option<&PauseReceipt> {
        match self {
            Self::Unpaused => None,
            Self::Paused { receipt }
            | Self::FaultedPaused { receipt, .. }
            | Self::RecoveryHealthUnconfirmed { receipt }
            | Self::Released { receipt, .. }
            | Self::ConsumedByHandoff { receipt, .. } => Some(receipt),
        }
    }

    pub fn is_paused(&self) -> bool {
        matches!(
            self,
            Self::Paused { .. }
                | Self::FaultedPaused { .. }
                | Self::RecoveryHealthUnconfirmed { .. }
        )
    }

    /// A durable claim can only describe a quiescent checkpoint. Never recover an impossible
    /// combination by dropping its marker or guessing which provider work completed.
    pub(crate) fn valid_checkpoint(&self, snapshot: &super::snapshot::HarnessSnapshot) -> bool {
        if !self.is_paused() {
            return true;
        }
        use super::{HarnessPhaseTag, compact_outcome::CompactStatus};
        matches!(
            snapshot.phase,
            HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted
        ) && snapshot.wedged_reason.is_none()
            && !snapshot
                .pending_entries()
                .iter()
                .any(super::QueueEntry::is_user_authored)
            && snapshot.projection_client_id.is_none()
            && snapshot.pending_rewind.is_none()
            && !snapshot.compact_outcome.as_ref().is_some_and(|outcome| {
                matches!(
                    outcome.status,
                    CompactStatus::Started | CompactStatus::Unconfirmed
                )
            })
    }
}

/// A legacy unpaused row has no evidence about any release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PauseQuery {
    Unknown,
    Recorded(PauseState),
}

impl PauseState {
    /// Only active claims have a release transition; absence is never healthy evidence.
    pub(crate) fn release_active_claim(&self) -> Option<Self> {
        let (receipt, outcome) = match self {
            Self::Paused { receipt } => (receipt, ReleaseOutcome::HealthyReleased),
            Self::FaultedPaused { receipt, fault } => (
                receipt,
                ReleaseOutcome::ReleasedWithBackendFault {
                    fault: fault.clone(),
                },
            ),
            Self::RecoveryHealthUnconfirmed { receipt } => {
                (receipt, ReleaseOutcome::HealthUnconfirmedAfterRecovery)
            }
            Self::Unpaused | Self::Released { .. } | Self::ConsumedByHandoff { .. } => return None,
        };
        Some(Self::Released {
            receipt: receipt.clone(),
            outcome,
        })
    }

    /// A rebuilt owner has no proof about its predecessor's provider health.
    pub(crate) fn unconfirm_recovered_health(&mut self) {
        if let Self::Paused { receipt } = self {
            *self = Self::RecoveryHealthUnconfirmed {
                receipt: receipt.clone(),
            };
        }
    }
}

impl ReleaseOutcome {
    pub(crate) fn failed_state(&self) -> Option<super::HarnessState> {
        match self {
            Self::HealthyReleased => None,
            Self::ReleasedWithBackendFault { fault } => Some(fault.released_state()),
            Self::HealthUnconfirmedAfterRecovery => Some(super::HarnessState::Wedged {
                since: std::time::Instant::now(),
                reason: calm_types::harness::HARNESS_RECOVERY_HEALTH_UNCONFIRMED_REASON.into(),
            }),
        }
    }
}
