//! Immutable dispatch rendering: human content is loaded only through revision references.
use super::{
    AcceptedInputRef, LegacyInputBoundary, MessageRevisionRef, PrivateHistoryPayload,
    SystemObservationId,
};
use crate::model::HarnessInputPresentation;
use crate::observation::Observation;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A system observation cannot smuggle the legacy UserMessage payload into V2.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Observation")]
pub struct SystemObservation(Observation);

impl SystemObservation {
    pub fn observation(&self) -> &Observation {
        &self.0
    }
}

impl TryFrom<Observation> for SystemObservation {
    type Error = &'static str;

    fn try_from(value: Observation) -> Result<Self, Self::Error> {
        if matches!(value, Observation::UserMessage { .. }) {
            return Err("human text requires a canonical message revision");
        }
        Ok(Self(value))
    }
}

/// Frozen attribution is display data; current membership is checked separately.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenHumanAttribution {
    pub registered_user_id: String,
    pub display_name: String,
}

/// Source and payload are coupled: only a System variant can own observation content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubmissionRenderInput {
    Human {
        message: MessageRevisionRef,
        author: FrozenHumanAttribution,
    },
    System {
        observation_id: SystemObservationId,
        payload: PrivateHistoryPayload<SystemObservation>,
    },
    Legacy {
        message: MessageRevisionRef,
        source_locator: String,
        boundary: LegacyInputBoundary,
    },
}

impl SubmissionRenderInput {
    pub fn input_ref(&self) -> AcceptedInputRef {
        match self {
            Self::Human { message, .. } => AcceptedInputRef::Human {
                message: message.clone(),
            },
            Self::System { observation_id, .. } => AcceptedInputRef::System {
                observation_id: observation_id.clone(),
            },
            Self::Legacy { message, .. } => AcceptedInputRef::Legacy {
                message: message.clone(),
            },
        }
    }
}

/// Trusted per-turn context is fixed at claim time, and is erased with its submission.
/// It cannot acquire human attribution by choosing User presentation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SystemRenderContextWire")]
pub struct SystemRenderContext {
    presentation: HarnessInputPresentation,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SystemRenderContextWire {
    presentation: HarnessInputPresentation,
    text: String,
}

impl SystemRenderContext {
    pub fn new(presentation: HarnessInputPresentation, text: String) -> Result<Self, &'static str> {
        if presentation == HarnessInputPresentation::User {
            return Err("system context cannot have human presentation");
        }
        Ok(Self { presentation, text })
    }

    pub fn presentation(&self) -> HarnessInputPresentation {
        self.presentation
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl TryFrom<SystemRenderContextWire> for SystemRenderContext {
    type Error = &'static str;

    fn try_from(value: SystemRenderContextWire) -> Result<Self, Self::Error> {
        Self::new(value.presentation, value.text)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubmissionRenderSegment {
    Input {
        input_position: u32,
        input: SubmissionRenderInput,
    },
    Context {
        payload: PrivateHistoryPayload<SystemRenderContext>,
    },
}

/// Internal storage format; V1 names this plan, not the retired body-bearing snapshot.
/// The original digest is stored alongside it and is not recomputed after erasure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "version", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubmissionRenderPlan {
    V1 {
        segments: Vec<SubmissionRenderSegment>,
    },
}

impl SubmissionRenderPlan {
    pub fn segments(&self) -> &[SubmissionRenderSegment] {
        match self {
            Self::V1 { segments } => segments,
        }
    }

    /// Called by the canonical claim/read owner before persisting or hydrating a plan.
    pub fn input_refs(&self) -> Result<Vec<AcceptedInputRef>, &'static str> {
        let mut inputs = Vec::new();
        let mut seen = HashSet::new();
        for segment in self.segments() {
            if let SubmissionRenderSegment::Input {
                input_position,
                input,
            } = segment
            {
                if usize::try_from(*input_position).ok() != Some(inputs.len()) {
                    return Err("submission input positions must be contiguous and ordered");
                }
                let input = input.input_ref();
                if !seen.insert(input.identity()) {
                    return Err("submission cannot claim an input twice");
                }
                inputs.push(input);
            }
        }
        if inputs.is_empty() {
            return Err("submission must claim at least one input");
        }
        Ok(inputs)
    }
}
