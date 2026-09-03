//! Persisted `PlannerHarness` state.
//!
//! `schema_version = 1` is the first live harness schema. Future schema bumps
//! must migrate rows in the boot recovery path before tasks are respawned. The
//! recovery contract is deliberately strict: the kernel must know every live
//! schema it may encounter, so an unknown `schema_version` panics with
//! `unknown PlannerHarness snapshot schema_version {n}; boot recovery must migrate live schemas`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::Observation;
use crate::harness::state::{HarnessState, IssuingKind};
use crate::harness::token_usage::TokenUsage;

// #679 PR1 — `HarnessPhaseTag` moved to `calm_types::harness` (TS-exported,
// referenced by `Event::HarnessPhaseChanged`). Re-exported so the
// `crate::harness::snapshot::HarnessPhaseTag` path is unchanged. The
// `From<&HarnessState>` impl below stays here — `HarnessState` is local.
pub use calm_types::harness::HarnessPhaseTag;

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
    pub issued_turn_head: Option<String>,
    #[serde(default)]
    pub wedged_reason: Option<String>,
    /// #1255 S3 — latest `thread/tokenUsage/updated` reading for this thread.
    ///
    /// No `schema_version` bump for this field, and that is a checked claim,
    /// not an assumption. `HarnessSnapshot` carries no
    /// `#[serde(deny_unknown_fields)]` (nor does any type it nests), so the
    /// two directions are:
    ///
    /// - **new binary reading an old snapshot**: the key is absent,
    ///   `#[serde(default)]` supplies `None`, and `assert_known_schema` only
    ///   ever compares the integer — which is unchanged.
    /// - **old binary reading a new snapshot** (the rollback direction, and
    ///   the one that actually forces a version bump when it fails): serde's
    ///   default is to *ignore* unknown keys, so an old build drops
    ///   `token_usage` and boots. It loses the reading, which is the correct
    ///   loss for a value that is re-pushed on the next model response.
    ///
    /// Bumping the version for a purely additive, defaulted field would have
    /// cost the opposite: `assert_known_schema` panics on an unknown version,
    /// so a bump makes every live snapshot unreadable by the older binary —
    /// it would turn a lossless rollback into a boot panic.
    ///
    /// The first direction is now a *tested* claim, not only a read one:
    /// `a_pre_1255_snapshot_without_token_usage_still_deserializes` below
    /// feeds a literal that omits the key. It has to be a literal — every
    /// other call site in the suite hands `from_value_strict` JSON that a new
    /// binary just serialized, so `token_usage` is always present and the
    /// absent-key path is otherwise never exercised.
    ///
    /// What that test actually pins, measured rather than assumed: deleting
    /// `#[serde(default)]` from this field changes **nothing**, because
    /// serde's derive already treats a missing `Option<T>` field as `None`.
    /// The attribute is belt-and-braces and the test stays green without it —
    /// verified by removing it and re-running. What *does* redden the test is
    /// the field becoming genuinely required (a `deserialize_with`, or a
    /// non-`Option` type), and the failure is not a missing field: it is
    /// `is_harness_snapshot_value` answering false for every pre-#1255 row,
    /// and `HarnessSnapshot::from_value_strict` **panicking** in boot recovery
    /// (`harness/mod.rs`, which unlike `routes/cards.rs` has no
    /// pre-validation guard) — i.e. every existing harness unrecoverable on
    /// upgrade. That is the mutation the test was verified against.
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
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
            issued_turn_head: None,
            wedged_reason: None,
            token_usage: None,
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
            issued_turn_head: None,
            // Set by `snapshot_for` from `Inner`, exactly like
            // `last_seen_head` / `issued_turn_head` above: `from_state` sees
            // only `HarnessState`, and token usage does not live there.
            token_usage: None,
            wedged_reason,
        }
    }

    pub fn from_value_strict(value: Value) -> Self {
        let mut snapshot: Self =
            serde_json::from_value(value).expect("deserialize PlannerHarness snapshot");
        snapshot.assert_known_schema();
        snapshot.align_pending_envelope_ids();
        snapshot
    }

    pub fn assert_known_schema(&self) {
        assert!(
            self.schema_version == HARNESS_SNAPSHOT_SCHEMA_VERSION,
            "unknown PlannerHarness snapshot schema_version {}; boot recovery must migrate live schemas",
            self.schema_version
        );
        assert!(
            self.mode == HARNESS_MODE,
            "invalid PlannerHarness snapshot mode {}; expected harness",
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Forward compatibility as an executed test rather than an asserted
    /// claim (#1255 S3 review).
    ///
    /// This literal is a snapshot as a **pre-#1255 binary wrote it**: no
    /// `token_usage` key at all. Nothing else in the suite can catch a
    /// regression here, because every other `from_value_strict` call site
    /// feeds it JSON a *current* binary just serialized, in which the key is
    /// always present.
    ///
    /// Mutation-verified: removing `#[serde(default)]` alone does NOT redden
    /// it (serde already reads a missing `Option` field as `None`), but making
    /// the field genuinely required does — `#[serde(deserialize_with =
    /// "Option::<TokenUsage>::deserialize")]` fails the
    /// `is_harness_snapshot_value` assertion below. That red is the same red
    /// as every deployed harness failing to recover on upgrade: boot recovery
    /// (`harness/mod.rs`) calls `from_value_strict` with no pre-validation, so
    /// it would panic.
    #[test]
    fn a_pre_1255_snapshot_without_token_usage_still_deserializes() {
        let pre_1255 = json!({
            "schema_version": HARNESS_SNAPSHOT_SCHEMA_VERSION,
            "mode": HARNESS_MODE,
            "phase": "idle",
            "push_watermark": 42,
            "pending_queue": [],
            "pending_envelope_ids": [],
            "last_thread_id": "thread-pre-1255",
            "last_turn_id": null,
            "last_report_body_sha256": null,
            "last_seen_head": null,
            "issued_turn_head": null,
            "wedged_reason": null
        });
        assert!(
            pre_1255.get("token_usage").is_none(),
            "the point of this literal is the ABSENT key; do not add it"
        );

        assert!(
            is_harness_snapshot_value(&pre_1255),
            "a pre-#1255 row must still be recognised as a harness snapshot — \
             `routes::cards::get_planner_run` uses this to decide dormant-vs-live"
        );

        let snapshot = HarnessSnapshot::from_value_strict(pre_1255);
        assert_eq!(
            snapshot.token_usage, None,
            "an absent reading defaults to None, not to a zeroed reading"
        );
        assert_eq!(snapshot.push_watermark, 42, "the rest still round-trips");
        assert_eq!(snapshot.last_thread_id.as_deref(), Some("thread-pre-1255"));
    }
}
