//! Version two domain progress. Required evidence is carried only by its owning state.
use super::handoff_activation::ActivationResult;
use super::handoff_coordinator::CoordinatorInput;
use super::handoff_cutover::{
    CutoverConfiguration, CutoverResult, CutoverScope, ValidatedCutoverPlan,
};
use crate::harness::handoff_pause::{PauseReceipt, ReleaseOutcome};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Claim {
    Untouched,
    Intent,
    Paused {
        receipt: PauseReceipt,
    },
    Released {
        receipt: PauseReceipt,
        outcome: ReleaseOutcome,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Claims {
    pub source: Claim,
    pub target: Claim,
}
impl Claims {
    pub fn empty() -> Self {
        Self {
            source: Claim::Untouched,
            target: Claim::Untouched,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommittedIdentity {
    pub origin: IdentityOrigin,
    pub configuration: CutoverConfiguration,
    pub source_pause: PauseReceipt,
    pub target_pause: PauseReceipt,
    pub push_watermark: i64,
}
impl CommittedIdentity {
    pub(in crate::operation) fn plan(&self, input: &CoordinatorInput) -> ValidatedCutoverPlan {
        ValidatedCutoverPlan {
            configuration: self.configuration.clone(),
            scope: CutoverScope {
                track_id: input.request.track_id.clone(),
                source_card_id: input.request.source_card_id.clone(),
                source_session_id: input.request.source_session_id.clone(),
                target_card_id: input.request.target_card_id.clone(),
                target_session_id: input.request.target_session_id.clone(),
                source_pause: self.source_pause.clone(),
                target_pause: self.target_pause.clone(),
                plan_id: input.plan_id.clone(),
                new_session_id: input.new_session_id.clone(),
                user_audit_id: input.request.user_audit_id.clone(),
            },
        }
    }
    pub(in crate::operation) fn result(&self, input: &CoordinatorInput, id: &str) -> CutoverResult {
        CutoverResult::CutoverCommittedPendingActivation {
            operation_id: id.into(),
            plan_id: input.plan_id.clone(),
            track_id: input.request.track_id.clone(),
            history_card_id: input.request.source_card_id.clone(),
            retired_source_session_id: input.request.source_session_id.clone(),
            retired_target_session_id: input.request.target_session_id.clone(),
            planner_card_id: input.request.target_card_id.clone(),
            planner_session_id: input.new_session_id.clone(),
            configuration: self.configuration.clone(),
            snapshot_session_id: input.new_session_id.clone(),
            push_watermark: self.push_watermark,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Resources {
    Unprepared,
    Prepared,
    NativeIntent,
    Unknown {
        reason: String,
    },
    Proven {
        thread_id: String,
    },
    Blocked {
        reason: String,
        resume: ResourceResume,
    },
}
impl Resources {
    pub fn result(&self, diagnostic: Option<&str>) -> ActivationResult {
        if matches!(
            self,
            Self::Unprepared | Self::Prepared | Self::Proven { .. }
        ) && let Some(reason) = diagnostic
        {
            return ActivationResult::NeedsRecovery {
                reason: reason.into(),
            };
        }
        match self {
            Self::Unprepared | Self::Prepared => ActivationResult::Preparing,
            Self::NativeIntent => ActivationResult::Unknown {
                reason: "Native resource result unconfirmed; never resend".into(),
            },
            Self::Unknown { reason } => ActivationResult::Unknown {
                reason: reason.clone(),
            },
            Self::Proven { thread_id } => ActivationResult::ResourcesCheckpointed {
                thread_id: thread_id.clone(),
            },
            Self::Blocked { reason, resume } => ActivationResult::Blocked {
                reason: reason.clone(),
                resources_checkpointed: matches!(resume, ResourceResume::Proven { .. }),
            },
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Progress {
    Reserved,
    Pausing {
        claims: Claims,
    },
    Compensating {
        claims: Claims,
        reason: String,
    },
    Finished {
        claims: SettledClaims,
    },
    Committed {
        identity: CommittedIdentity,
        resources: Resources,
    },
    Activated {
        identity: CommittedIdentity,
    },
}
impl Progress {
    pub fn reserved() -> Self {
        Self::Reserved
    }
    pub fn terminal(&self) -> bool {
        matches!(self, Self::Finished { .. } | Self::Activated { .. })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "version", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum IdentityOrigin {
    Workflow,
    Legacy { cutover_operation_id: String },
}
impl CommittedIdentity {
    pub fn evidence_owner<'a>(&'a self, owner_id: &'a str) -> &'a str {
        match &self.origin {
            IdentityOrigin::Workflow => owner_id,
            IdentityOrigin::Legacy {
                cutover_operation_id,
            } => cutover_operation_id,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ResourceResume {
    Prepared,
    Proven { thread_id: String },
}

/// A finished compensation cannot carry an intent, a paused claim or unhealthy release.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettledClaims {
    pub source: SettledClaim,
    pub target: SettledClaim,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SettledClaim {
    Untouched,
    Released {
        receipt: PauseReceipt,
        outcome: HealthyRelease,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HealthyRelease {
    HealthyReleased,
}
impl TryFrom<Claims> for SettledClaims {
    type Error = crate::error::CalmError;
    fn try_from(claims: Claims) -> Result<Self, Self::Error> {
        Ok(Self {
            source: claims.source.try_into()?,
            target: claims.target.try_into()?,
        })
    }
}
impl TryFrom<Claim> for SettledClaim {
    type Error = crate::error::CalmError;
    fn try_from(claim: Claim) -> Result<Self, Self::Error> {
        match claim {
            Claim::Untouched => Ok(Self::Untouched),
            Claim::Released {
                receipt,
                outcome: ReleaseOutcome::HealthyReleased,
            } => Ok(Self::Released {
                receipt,
                outcome: HealthyRelease::HealthyReleased,
            }),
            _ => Err(super::handoff_cutover::conflict()),
        }
    }
}
