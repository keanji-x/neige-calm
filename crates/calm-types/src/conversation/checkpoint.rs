//! Facts held by the existing harness snapshot. This is not a second outcome
//! ledger or execution driver. Provider-private evidence is checked before the
//! execution owner records a positive fact; requests must never supply it.
use super::{
    ActiveInputSubmissions, InputAttempt, InputSubmissionId, PendingInput, validate_pending_inputs,
};
use crate::runtime::AgentProvider;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use ts_rs::TS;
use utoipa::ToSchema;

pub const CONVERSATION_INPUT_CHECKPOINT_KEY: &str = "canonical_inputs";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputCorrelationFact {
    pub provider: AgentProvider,
    pub thread_id: String,
    pub native_session_id: Option<String>,
    pub local_turn_id: Option<String>,
    pub native_turn_id: Option<String>,
    pub rpc_request_id: Option<u64>,
}

/// Positive receipt shape, not a provider capability table. The private adapter
/// or qualified native-event owner decides which receipt its provider proves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeInputAcceptance {
    NativeTurn { native_turn_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputDeliveryFact {
    /// Pre-call marker: a missing return is uncertain, never proof of no send.
    CrossingUnknown,
    /// An observed uncertain report, which a later local refusal cannot clear.
    Unknown,
    /// `basis` is an adapter-owned protocol tag, never an error/diagnostic body.
    NotDispatched { basis: String },
    Accepted {
        basis: String,
        evidence: NativeInputAcceptance,
    },
}

/// Exact choices passed to the native invocation. Absent model/effort keys mean
/// inherit the thread's value, not a reconstruction from later card settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct FrozenInputInvocation {
    #[schema(required = true, nullable = true)]
    #[serde(deserialize_with = "Option::deserialize")]
    pub model: Option<String>,
    #[schema(required = true, nullable = true)]
    #[serde(deserialize_with = "Option::deserialize")]
    pub effort: Option<String>,
    pub permission: crate::harness::PlannerPermissionMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum InputInvocation {
    Recorded {
        configuration: FrozenInputInvocation,
    },
    PriorInvocationNotRecorded,
}

/// An adapter-owned observed fact. Revision and frozen invocation belong to the
/// checkpoint owner and cannot be supplied or replaced by a native report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputAttemptFact {
    pub attempt: InputAttempt,
    pub correlation: InputCorrelationFact,
    pub delivery: InputDeliveryFact,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputAttemptCheckpoint {
    pub revision: u64,
    pub invocation: InputInvocation,
    pub attempt: InputAttempt,
    pub correlation: InputCorrelationFact,
    pub delivery: InputDeliveryFact,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationInputCheckpoint {
    /// Private queue order, unrelated to presentation or native rewind anchors.
    /// Claims retain the removed entry's position for exact ordered release.
    pub next_queue_position: u64,
    pub pending: Vec<PendingInput>,
    pub active: Option<ActiveInputSubmissions>,
    /// Lookup-only correlation facts. Ordering never chooses work to execute.
    /// At most the latest attempt of each immutable submission is retained.
    pub attempts: Vec<InputAttemptCheckpoint>,
    pub receipts: Vec<super::HumanKeepReceipt>,
}

impl ConversationInputCheckpoint {
    pub fn empty() -> Self {
        Self {
            next_queue_position: 0,
            pending: vec![],
            active: None,
            attempts: vec![],
            receipts: vec![],
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        validate_pending_inputs(&self.pending)?;
        if self
            .pending
            .last()
            .is_some_and(|entry| entry.queue_position() >= self.next_queue_position)
        {
            return Err("pending queue position exceeds its allocation counter");
        }
        if let Some(active) = &self.active {
            let mut seen = HashSet::new();
            if active.submission_ids.is_empty()
                || active.submission_ids.iter().any(|id| !seen.insert(id))
            {
                return Err("active batch must reference distinct immutable submissions");
            }
        }
        let mut submissions = HashSet::new();
        let mut attempts = HashSet::new();
        for checkpoint in &self.attempts {
            checkpoint.validate()?;
            if !submissions.insert(&checkpoint.attempt.submission_id)
                || !attempts.insert(&checkpoint.attempt.attempt_id)
            {
                return Err("snapshot repeats an input submission or attempt identity");
            }
        }
        let mut keys = HashSet::new();
        let mut decisions = HashSet::new();
        for receipt in &self.receipts {
            if receipt.key.is_empty()
                || receipt.principal.is_empty()
                || receipt.fingerprint.len() != 64
                || !keys.insert(&receipt.key)
                || !decisions.insert(&receipt.result.attempt_id)
                || receipt.request.carrier_id != receipt.result.carrier_id
                || receipt.request.submission_id != receipt.result.submission_id
                || receipt.request.attempt_id != receipt.result.attempt_id
            {
                return Err("invalid Human Keep receipt identity");
            }
            let prior = receipt
                .request
                .revision
                .parse::<u64>()
                .map_err(|_| "invalid Keep revision")?;
            let next = prior.checked_add(1).ok_or("Keep revision overflow")?;
            if prior.to_string() != receipt.request.revision
                || next.to_string() != receipt.result.revision
                || !self.attempts.iter().any(|a| {
                    a.attempt.attempt_id.as_str() == receipt.result.attempt_id
                        && a.attempt.submission_id.as_str() == receipt.result.submission_id
                        && a.revision >= next
                })
            {
                return Err("Human Keep receipt disagrees with its attempt");
            }
        }
        Ok(())
    }

    /// Checkpoint the crossing before a call. An older Unknown or Accepted
    /// attempt cannot be displaced by a newly refused attempt or generic retry.
    pub fn begin_attempt(
        &mut self,
        attempt: InputAttempt,
        correlation: InputCorrelationFact,
        invocation: FrozenInputInvocation,
    ) -> Result<(), &'static str> {
        self.validate()?;
        let marker = InputAttemptCheckpoint {
            revision: 0,
            invocation: InputInvocation::Recorded {
                configuration: invocation,
            },
            attempt,
            correlation,
            delivery: InputDeliveryFact::CrossingUnknown,
        };
        marker.validate()?;
        if self
            .attempts
            .iter()
            .any(|prior| prior.attempt.attempt_id == marker.attempt.attempt_id)
        {
            return Err("dispatch attempt identity cannot be reused");
        }
        if let Some(prior) = self
            .attempts
            .iter_mut()
            .find(|prior| prior.attempt.submission_id == marker.attempt.submission_id)
        {
            if !matches!(prior.delivery, InputDeliveryFact::NotDispatched { .. }) {
                return Err("an unresolved or accepted submission cannot dispatch again");
            }
            *prior = marker;
        } else {
            self.attempts.push(marker);
        }
        Ok(())
    }

    /// The execution owner calls this only after validating the private backend
    /// report, or an exact late native receipt through its existing ingress.
    pub fn record_attempt_fact(&mut self, report: InputAttemptFact) -> Result<(), &'static str> {
        self.validate()?;
        report.validate()?;
        if matches!(report.delivery, InputDeliveryFact::CrossingUnknown) {
            return Err("only begin_attempt can record a crossing marker");
        }
        let prior = self
            .attempts
            .iter_mut()
            .find(|prior| prior.attempt.submission_id == report.attempt.submission_id)
            .ok_or("provider report has no durable crossing marker")?;
        if prior.attempt == report.attempt
            && prior.correlation == report.correlation
            && prior.delivery == report.delivery
        {
            return Ok(());
        }
        if prior.attempt != report.attempt
            || !matches!(
                prior.delivery,
                InputDeliveryFact::CrossingUnknown | InputDeliveryFact::Unknown
            )
            || (matches!(prior.delivery, InputDeliveryFact::Unknown)
                && matches!(report.delivery, InputDeliveryFact::NotDispatched { .. }))
        {
            return Err("provider report cannot settle a different or already settled attempt");
        }
        if prior.correlation.provider != report.correlation.provider
            || prior.correlation.thread_id != report.correlation.thread_id
            || !can_refine(
                &prior.correlation.native_session_id,
                &report.correlation.native_session_id,
            )
            || !can_refine(
                &prior.correlation.local_turn_id,
                &report.correlation.local_turn_id,
            )
            || !can_refine(
                &prior.correlation.native_turn_id,
                &report.correlation.native_turn_id,
            )
            || !can_refine(
                &prior.correlation.rpc_request_id,
                &report.correlation.rpc_request_id,
            )
        {
            return Err("provider report crosses the checkpoint's native correlation");
        }
        let revision = prior
            .revision
            .checked_add(1)
            .ok_or("input attempt revision exhausted")?;
        prior.attempt = report.attempt;
        prior.correlation = report.correlation;
        prior.delivery = report.delivery;
        prior.revision = revision;
        Ok(())
    }

    /// Display identities: current execution plus retained uncertain attempts,
    /// once each. Ended positive receipts remain in storage, outside this view.
    /// This order never chooses executable work.
    pub fn referenced_submissions(&self) -> Vec<InputSubmissionId> {
        let mut ids = self
            .active
            .as_ref()
            .map(|active| active.submission_ids.clone())
            .unwrap_or_default();
        for attempt in &self.attempts {
            match &attempt.delivery {
                InputDeliveryFact::CrossingUnknown | InputDeliveryFact::Unknown => {
                    if !self.is_kept(&attempt.attempt)
                        && !ids.contains(&attempt.attempt.submission_id)
                    {
                        ids.push(attempt.attempt.submission_id.clone());
                    }
                }
                InputDeliveryFact::NotDispatched { .. } | InputDeliveryFact::Accepted { .. } => {}
            }
        }
        ids
    }

    /// Only a receipt can retire an input blocker. An addressable original
    /// execution instead remains bound until its qualified END.
    pub fn requires_settled_activity(&self, current_thread: &str) -> Result<bool, &'static str> {
        let mut required = false;
        for attempt in &self.attempts {
            if self.is_kept(&attempt.attempt)
                && attempt.correlation.native_turn_id.is_none()
                && attempt.correlation.local_turn_id.is_none()
            {
                if attempt.correlation.thread_id != current_thread {
                    return Err("kept input requires settlement of its original exact thread");
                }
                required = true;
            }
        }
        Ok(required)
    }

    pub fn kept_execution_without_target(&self) -> bool {
        self.active.as_ref().is_some_and(|active| {
            active.submission_ids.iter().all(|id| {
                self.attempt(id).is_some_and(|attempt| {
                    self.is_kept(&attempt.attempt)
                        && attempt.correlation.native_turn_id.is_none()
                        && attempt.correlation.local_turn_id.is_none()
                })
            })
        })
    }

    pub fn is_kept(&self, attempt: &InputAttempt) -> bool {
        self.receipts.iter().any(|receipt| {
            receipt.result.submission_id == attempt.submission_id.as_str()
                && receipt.result.attempt_id == attempt.attempt_id.as_str()
        })
    }

    pub fn attempt(&self, submission: &InputSubmissionId) -> Option<&InputAttemptCheckpoint> {
        self.attempts
            .iter()
            .find(|checkpoint| &checkpoint.attempt.submission_id == submission)
    }
}

impl InputAttemptCheckpoint {
    fn validate(&self) -> Result<(), &'static str> {
        validate_fact(&self.correlation, &self.delivery)
    }
}
impl InputAttemptFact {
    fn validate(&self) -> Result<(), &'static str> {
        validate_fact(&self.correlation, &self.delivery)
    }
}
fn validate_fact(
    correlation: &InputCorrelationFact,
    delivery: &InputDeliveryFact,
) -> Result<(), &'static str> {
    if correlation.thread_id.is_empty() {
        return Err("dispatch correlation requires a thread identity");
    }
    for value in [
        &correlation.native_session_id,
        &correlation.local_turn_id,
        &correlation.native_turn_id,
    ]
    .into_iter()
    .flatten()
    {
        if value.is_empty() {
            return Err("native correlation cannot contain empty identities");
        }
    }
    match delivery {
        InputDeliveryFact::NotDispatched { basis } if basis.is_empty() => {
            Err("non-dispatch fact requires its adapter basis")
        }
        InputDeliveryFact::Accepted { basis, evidence } => {
            if basis.is_empty() {
                return Err("accepted fact requires its adapter basis");
            }
            match evidence {
                NativeInputAcceptance::NativeTurn { native_turn_id }
                    if native_turn_id.is_empty()
                        || correlation.native_turn_id.as_ref() != Some(native_turn_id) =>
                {
                    Err("native turn receipt requires matching positive native correlation")
                }
                NativeInputAcceptance::NativeTurn { .. } => Ok(()),
            }
        }
        InputDeliveryFact::NotDispatched { .. }
        | InputDeliveryFact::CrossingUnknown
        | InputDeliveryFact::Unknown => Ok(()),
    }
}

fn can_refine<T: PartialEq>(prior: &Option<T>, reported: &Option<T>) -> bool {
    prior
        .as_ref()
        .is_none_or(|value| reported.as_ref() == Some(value))
}
