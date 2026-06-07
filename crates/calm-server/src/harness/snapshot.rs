//! Persisted `SpecHarness` state.
//!
//! `schema_version = 1` is the first live harness schema. Future schema bumps
//! must migrate rows in the boot recovery path before tasks are respawned. The
//! recovery contract is deliberately strict: the kernel must know every live
//! schema it may encounter, so an unknown `schema_version` panics with
//! `unknown SpecHarness snapshot schema_version {n}; boot recovery must migrate live schemas`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::Observation;
use crate::harness::state::{HarnessState, IssuingKind};

pub const HARNESS_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const HARNESS_MODE: &str = "harness";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HarnessSnapshot {
    pub schema_version: u32,
    pub mode: String,
    pub phase: HarnessPhaseTag,
    #[serde(default)]
    pub push_watermark: i64,
    #[serde(default)]
    pub pending_queue: Vec<Observation>,
    #[serde(default)]
    pub last_thread_id: Option<String>,
    #[serde(default)]
    pub last_turn_id: Option<String>,
    #[serde(default)]
    pub last_report_body_sha256: Option<String>,
    #[serde(default)]
    pub wedged_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessPhaseTag {
    PendingThreadStart,
    Idle,
    IssuingTurn,
    IssuingInterrupt,
    TurnRunning,
    TurnCompleted,
    Resumed,
    Wedged,
}

impl HarnessSnapshot {
    pub fn initial(push_watermark: i64, pending_queue: Vec<Observation>) -> Self {
        Self {
            schema_version: HARNESS_SNAPSHOT_SCHEMA_VERSION,
            mode: HARNESS_MODE.to_string(),
            phase: HarnessPhaseTag::PendingThreadStart,
            push_watermark,
            pending_queue,
            last_thread_id: None,
            last_turn_id: None,
            last_report_body_sha256: None,
            wedged_reason: None,
        }
    }

    pub fn from_state(
        state: &HarnessState,
        push_watermark: i64,
        pending_queue: Vec<Observation>,
        last_thread_id: Option<String>,
        last_turn_id: Option<String>,
        last_report_body_sha256: Option<String>,
    ) -> Self {
        let phase = HarnessPhaseTag::from(state);
        let wedged_reason = match state {
            HarnessState::Wedged { reason, .. } => Some(reason.clone()),
            _ => None,
        };
        Self {
            schema_version: HARNESS_SNAPSHOT_SCHEMA_VERSION,
            mode: HARNESS_MODE.to_string(),
            phase,
            push_watermark,
            pending_queue,
            last_thread_id,
            last_turn_id,
            last_report_body_sha256,
            wedged_reason,
        }
    }

    pub fn from_value_strict(value: Value) -> Self {
        let snapshot: Self =
            serde_json::from_value(value).expect("deserialize SpecHarness snapshot");
        snapshot.assert_known_schema();
        snapshot
    }

    pub fn assert_known_schema(&self) {
        assert!(
            self.schema_version == HARNESS_SNAPSHOT_SCHEMA_VERSION,
            "unknown SpecHarness snapshot schema_version {}; boot recovery must migrate live schemas",
            self.schema_version
        );
        assert!(
            self.mode == HARNESS_MODE,
            "invalid SpecHarness snapshot mode {}; expected harness",
            self.mode
        );
    }
}

impl From<&HarnessState> for HarnessPhaseTag {
    fn from(state: &HarnessState) -> Self {
        match state {
            HarnessState::PendingThreadStart => Self::PendingThreadStart,
            HarnessState::Idle => Self::Idle,
            HarnessState::Issuing {
                kind: IssuingKind::TurnStart,
                ..
            } => Self::IssuingTurn,
            HarnessState::Issuing {
                kind: IssuingKind::Interrupt { .. },
                ..
            } => Self::IssuingInterrupt,
            HarnessState::TurnRunning { .. } => Self::TurnRunning,
            HarnessState::TurnCompleted { .. } => Self::TurnCompleted,
            HarnessState::Resumed { .. } => Self::Resumed,
            HarnessState::Wedged { .. } => Self::Wedged,
        }
    }
}
