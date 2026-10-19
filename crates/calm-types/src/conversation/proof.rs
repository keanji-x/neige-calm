//! Payload-free identity envelope for the existing provider submission journal.
//! This proves correlation only; it never proves native delivery or authority.
use super::{
    AcceptedInputRef, InputAttempt, InputSubmissionId, SubmissionRenderDigest, SubmissionRenderPlan,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionRenderPlanVersion {
    V1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmissionRenderPlanRef {
    pub version: SubmissionRenderPlanVersion,
    pub submission_id: InputSubmissionId,
    pub sha256: SubmissionRenderDigest,
}

/// V2 is the journal proof version, independent of the plan's own version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "version",
    rename_all = "snake_case",
    try_from = "SubmissionDispatchProofWire"
)]
pub enum SubmissionDispatchProof {
    V2 {
        attempt: InputAttempt,
        input_refs: Vec<AcceptedInputRef>,
        render_plan: SubmissionRenderPlanRef,
    },
}

#[derive(Deserialize)]
#[serde(tag = "version", rename_all = "snake_case", deny_unknown_fields)]
enum SubmissionDispatchProofWire {
    V2 {
        attempt: InputAttempt,
        input_refs: Vec<AcceptedInputRef>,
        render_plan: SubmissionRenderPlanRef,
    },
}

impl SubmissionRenderDigest {
    /// Called at initial claim before any payload erasure. Persist the result;
    /// later erasure does not change the original digest or journal proof.
    pub fn for_plan(plan: &SubmissionRenderPlan) -> Result<Self, serde_json::Error> {
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(plan)?));
        Ok(Self::parse(&digest).expect("SHA-256 formats as 64 lowercase hex characters"))
    }
}

impl SubmissionDispatchProof {
    /// Construct only for a fresh dispatch from the immutable, still-present
    /// stored plan. Recovery reads the original envelope and compares its digest
    /// to the stored original digest, never to a reserialized erased plan.
    pub fn from_stored_plan(
        attempt: InputAttempt,
        plan: &SubmissionRenderPlan,
        original_digest: SubmissionRenderDigest,
    ) -> Result<Self, String> {
        let input_refs = plan.input_refs().map_err(str::to_owned)?;
        if SubmissionRenderDigest::for_plan(plan).map_err(|error| error.to_string())?
            != original_digest
        {
            return Err("submission plan no longer matches its original digest".into());
        }
        let render_plan = SubmissionRenderPlanRef {
            version: SubmissionRenderPlanVersion::V1,
            submission_id: attempt.submission_id.clone(),
            sha256: original_digest,
        };
        Self::checked(attempt, input_refs, render_plan)
    }

    fn checked(
        attempt: InputAttempt,
        input_refs: Vec<AcceptedInputRef>,
        render_plan: SubmissionRenderPlanRef,
    ) -> Result<Self, String> {
        if attempt.submission_id != render_plan.submission_id {
            return Err("journal attempt and render plan name different submissions".into());
        }
        let mut identities = HashSet::new();
        for input in &input_refs {
            if !identities.insert(input.identity()) {
                return Err("journal proof repeats an accepted input identity".into());
            }
        }
        if input_refs.is_empty() {
            return Err("journal proof must name accepted inputs".into());
        }
        Ok(Self::V2 {
            attempt,
            input_refs,
            render_plan,
        })
    }

    pub fn attempt(&self) -> &InputAttempt {
        match self {
            Self::V2 { attempt, .. } => attempt,
        }
    }
    pub fn input_refs(&self) -> &[AcceptedInputRef] {
        match self {
            Self::V2 { input_refs, .. } => input_refs,
        }
    }
    pub fn render_plan(&self) -> &SubmissionRenderPlanRef {
        match self {
            Self::V2 { render_plan, .. } => render_plan,
        }
    }
}

impl TryFrom<SubmissionDispatchProofWire> for SubmissionDispatchProof {
    type Error = String;
    fn try_from(value: SubmissionDispatchProofWire) -> Result<Self, Self::Error> {
        match value {
            SubmissionDispatchProofWire::V2 {
                attempt,
                input_refs,
                render_plan,
            } => Self::checked(attempt, input_refs, render_plan),
        }
    }
}
