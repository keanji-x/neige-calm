//! Facts held by the existing harness snapshot. This is not a second outcome
//! ledger or execution driver. Provider-private evidence is checked before the
//! execution owner records a positive fact; requests must never supply it.
use super::{
    ActiveInputSubmissions, InputAttempt, InputSubmissionId, PendingInput, validate_pending_inputs,
};
use crate::runtime::AgentProvider;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputAttemptCheckpoint {
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
}

impl ConversationInputCheckpoint {
    pub fn empty() -> Self {
        Self {
            next_queue_position: 0,
            pending: vec![],
            active: None,
            attempts: vec![],
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
        Ok(())
    }

    /// Checkpoint the crossing before a call. An older Unknown or Accepted
    /// attempt cannot be displaced by a newly refused attempt or generic retry.
    pub fn begin_attempt(
        &mut self,
        attempt: InputAttempt,
        correlation: InputCorrelationFact,
    ) -> Result<(), &'static str> {
        self.validate()?;
        let marker = InputAttemptCheckpoint {
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
    pub fn record_attempt_fact(
        &mut self,
        report: InputAttemptCheckpoint,
    ) -> Result<(), &'static str> {
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
        if prior == &report {
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
        *prior = report;
        Ok(())
    }

    pub fn attempt(&self, submission: &InputSubmissionId) -> Option<&InputAttemptCheckpoint> {
        self.attempts
            .iter()
            .find(|checkpoint| &checkpoint.attempt.submission_id == submission)
    }
}

impl InputAttemptCheckpoint {
    fn validate(&self) -> Result<(), &'static str> {
        if self.correlation.thread_id.is_empty() {
            return Err("dispatch correlation requires a thread identity");
        }
        for value in [
            &self.correlation.native_session_id,
            &self.correlation.local_turn_id,
            &self.correlation.native_turn_id,
        ]
        .into_iter()
        .flatten()
        {
            if value.is_empty() {
                return Err("native correlation cannot contain empty identities");
            }
        }
        match &self.delivery {
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
                            || self.correlation.native_turn_id.as_ref() != Some(native_turn_id) =>
                    {
                        Err("native turn receipt requires matching positive native correlation")
                    }
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }
}

fn can_refine<T: PartialEq>(prior: &Option<T>, reported: &Option<T>) -> bool {
    prior
        .as_ref()
        .is_none_or(|value| reported.as_ref() == Some(value))
}
