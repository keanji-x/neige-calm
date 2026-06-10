//! Persisted `SpecHarness` state.
//!
//! `schema_version = 1` is the first live harness schema. Future schema bumps
//! must migrate rows in the boot recovery path before tasks are respawned. The
//! recovery contract is deliberately strict: the kernel must know every live
//! schema it may encounter, so an unknown `schema_version` panics with
//! `unknown SpecHarness snapshot schema_version {n}; boot recovery must migrate live schemas`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use utoipa::ToSchema;

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
    pub pending_envelope_ids: Vec<Option<i64>>,
    #[serde(default)]
    pub last_thread_id: Option<String>,
    #[serde(default)]
    pub last_turn_id: Option<String>,
    #[serde(default)]
    pub last_report_body_sha256: Option<String>,
    #[serde(default)]
    pub last_seen_head: Option<String>,
    #[serde(default)]
    pub wedged_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
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
        let pending_envelope_ids = vec![None; pending_queue.len()];
        Self {
            schema_version: HARNESS_SNAPSHOT_SCHEMA_VERSION,
            mode: HARNESS_MODE.to_string(),
            phase: HarnessPhaseTag::PendingThreadStart,
            push_watermark,
            pending_queue,
            pending_envelope_ids,
            last_thread_id: None,
            last_turn_id: None,
            last_report_body_sha256: None,
            last_seen_head: None,
            wedged_reason: None,
        }
    }

    pub fn from_state(
        state: &HarnessState,
        push_watermark: i64,
        pending_queue: Vec<Observation>,
        pending_envelope_ids: Vec<Option<i64>>,
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
            pending_envelope_ids,
            last_thread_id,
            last_turn_id,
            last_report_body_sha256,
            last_seen_head: None,
            wedged_reason,
        }
    }

    pub fn from_value_strict(value: Value) -> Self {
        let mut snapshot: Self =
            serde_json::from_value(value).expect("deserialize SpecHarness snapshot");
        snapshot.assert_known_schema();
        snapshot.align_pending_envelope_ids();
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

    pub fn align_pending_envelope_ids(&mut self) {
        self.pending_envelope_ids
            .resize(self.pending_queue.len(), None);
        self.pending_envelope_ids.truncate(self.pending_queue.len());
    }
}

pub fn is_harness_snapshot_value(value: &Value) -> bool {
    match serde_json::from_value::<HarnessSnapshot>(value.clone()) {
        Ok(snapshot) => {
            snapshot.schema_version == HARNESS_SNAPSHOT_SCHEMA_VERSION
                && snapshot.mode == HARNESS_MODE
        }
        Err(_) => false,
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
