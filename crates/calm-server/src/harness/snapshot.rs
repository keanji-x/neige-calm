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
use crate::model::HarnessInputSegment;

// #679 PR1 — `HarnessPhaseTag` moved to `calm_types::harness` (TS-exported,
// referenced by `Event::HarnessPhaseChanged`). Re-exported so the
// `crate::harness::snapshot::HarnessPhaseTag` path is unchanged. The
// `From<&HarnessState>` impl below stays here — `HarnessState` is local.
pub use calm_types::harness::HarnessPhaseTag;

pub const HARNESS_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const HARNESS_MODE: &str = "harness";

/// Presentation metadata retained until the echoed input completes (or the
/// next input batch supersedes it).
/// Keeping the turn id with it prevents a late notification from inheriting a
/// newer turn's classification, and persisting it lets a harness recovered
/// mid-turn classify the remaining item notifications without reading English.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IssuedInputSegments {
    pub turn_id: String,
    pub segments: Vec<HarnessInputSegment>,
}

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
    /// #1449 — one set of message ids per `pending_queue` entry, so two copies
    /// of the same sentence can be told apart wherever they meet.
    ///
    /// **Identity only. Not a state machine.** These ids answer exactly one
    /// question — "is this the same instance I moved?" — and they must not grow
    /// a disposition, an accepted/canceled terminal state, or any other
    /// semantics. A queue transfer that cannot tell instances apart degrades
    /// into matching on message text, which is forgeable (two identical
    /// sentences) and is the shape this repository has already been hurt by.
    ///
    /// A **set** per entry, not one id, because `try_fold_pending_tail`
    /// concatenates two adjacent `UserMessage`s into a single entry under
    /// backpressure (#615 F3). One id per entry would have to discard one of
    /// the two, which is the very loss of identity the ids exist to prevent, so
    /// a fold unions the sets instead.
    ///
    /// Empty for every non-`UserMessage` entry, and for every entry that was
    /// enqueued before this field existed. An empty set means "enqueued before
    /// the upgrade, instance not distinguishable"; the give-back skips such an
    /// entry and leaves the message where it is, rather than falling back to
    /// comparing text. Registered as a KNOWN GAP: a pre-upgrade sentence
    /// stranded on a runtime whose mint later fails stays on the successor
    /// rather than being returned.
    #[serde(default)]
    pub pending_message_ids: Vec<Vec<String>>,
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
    pub issued_input_segments: Option<IssuedInputSegments>,
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

/// The queue and the arrays that run parallel to it, passed as one value.
///
/// #1449 — they are three views of the same list and are meaningless apart: an
/// entry's identity is its position in all three. Passing them separately let
/// `from_state` grow to eight arguments and, more to the point, let a caller
/// pass two of the three.
pub struct PendingQueueState {
    pub queue: Vec<Observation>,
    pub envelope_ids: Vec<Option<i64>>,
    pub message_ids: Vec<Vec<String>>,
}

impl HarnessSnapshot {
    pub fn initial(push_watermark: i64, pending_queue: Vec<Observation>) -> Self {
        let pending_envelope_ids = vec![None; pending_queue.len()];
        let pending_message_ids = vec![Vec::new(); pending_queue.len()];
        Self {
            schema_version: HARNESS_SNAPSHOT_SCHEMA_VERSION,
            mode: HARNESS_MODE.to_string(),
            phase: HarnessPhaseTag::PendingThreadStart,
            push_watermark,
            pending_queue,
            pending_envelope_ids,
            pending_message_ids,
            last_thread_id: None,
            last_turn_id: None,
            last_report_body_sha256: None,
            last_seen_head: None,
            issued_turn_head: None,
            issued_input_segments: None,
            wedged_reason: None,
            token_usage: None,
        }
    }

    pub fn from_state(
        state: &HarnessState,
        push_watermark: i64,
        pending: PendingQueueState,
        last_thread_id: Option<String>,
        last_turn_id: Option<String>,
        last_report_body_sha256: Option<String>,
    ) -> Self {
        let PendingQueueState {
            queue: pending_queue,
            envelope_ids: pending_envelope_ids,
            message_ids: pending_message_ids,
        } = pending;
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
            pending_message_ids,
            last_thread_id,
            last_turn_id,
            last_report_body_sha256,
            last_seen_head: None,
            issued_turn_head: None,
            issued_input_segments: None,
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
        snapshot.align_pending_side_arrays();
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

    /// Bring EVERY array that runs parallel to `pending_queue` back to its
    /// length.
    ///
    /// One function for both, and renamed from `align_pending_envelope_ids`
    /// deliberately: two aligners would mean every call site has to remember
    /// both, and a site that aligned one and forgot the other would not fail —
    /// it would pair ids with the wrong entries and hand the give-back the
    /// wrong messages to move.
    ///
    /// One function is not the same as no call sites to get wrong.
    /// `output_snapshot` decoded a snapshot with a bare `from_value` and skipped
    /// this entirely, for a whole review round, which is exactly the
    /// mis-attribution above with the aligner in place.
    ///
    /// This is also the upgrade seam: `from_value_strict` calls it on every
    /// snapshot decoded from the database, so a pre-#1449 row — which has no
    /// `pending_message_ids` at all and therefore decodes to an EMPTY outer vec
    /// against an N-entry queue — comes out with N empty sets rather than a
    /// length mismatch.
    pub fn align_pending_side_arrays(&mut self) {
        // `resize` both grows and shortens.
        self.pending_envelope_ids
            .resize(self.pending_queue.len(), None);
        self.pending_message_ids
            .resize(self.pending_queue.len(), Vec::new());
    }
}

#[cfg(test)]
mod pending_side_array_tests {
    use super::*;
    use crate::harness::observation::Observation;

    fn queued(texts: &[&str]) -> Vec<Observation> {
        texts
            .iter()
            .map(|text| Observation::UserMessage {
                text: (*text).to_string(),
            })
            .collect()
    }

    /// #1449 — a snapshot written before `pending_message_ids` existed decodes
    /// with an EMPTY outer vec against a non-empty queue.
    ///
    /// That length mismatch is worse than having no ids at all: any code that
    /// pairs the arrays by index would attribute an id to the wrong entry, and
    /// the give-back would then move the wrong sentence. `from_value_strict`
    /// aligns on the way in so the mismatch cannot leave the decoder.
    #[test]
    fn an_upgraded_snapshot_decodes_with_one_empty_id_set_per_entry() {
        let mut legacy = serde_json::to_value(HarnessSnapshot::initial(
            0,
            queued(&["one", "two", "three"]),
        ))
        .expect("serialize");
        legacy
            .as_object_mut()
            .expect("object")
            .remove("pending_message_ids");
        assert!(
            legacy.get("pending_message_ids").is_none(),
            "premise: the field is absent, exactly as a pre-#1449 row has it"
        );

        let snapshot = HarnessSnapshot::from_value_strict(legacy);
        assert_eq!(snapshot.pending_queue.len(), 3);
        assert_eq!(
            snapshot.pending_message_ids.len(),
            snapshot.pending_queue.len(),
            "the arrays must come out of the decoder the same length"
        );
        assert!(
            snapshot.pending_message_ids.iter().all(Vec::is_empty),
            "and every entry must say `no identity`, not borrow somebody else's"
        );
    }

    /// The counter-fixture: a deliberately mismatched snapshot must be
    /// CORRECTED by the aligner, not carried through.
    ///
    /// Both directions: a short array leaves entries with no id, a long one
    /// pairs ids with entries that do not exist, and either one is a
    /// mis-attribution the give-back would act on.
    #[test]
    fn alignment_corrects_both_a_short_and_a_long_id_array() {
        let mut short = HarnessSnapshot::initial(0, queued(&["one", "two", "three"]));
        short.pending_message_ids = vec![vec!["m1".into()]];
        short.pending_envelope_ids = vec![Some(1)];
        short.align_pending_side_arrays();
        assert_eq!(short.pending_message_ids.len(), 3);
        assert_eq!(short.pending_envelope_ids.len(), 3);
        assert_eq!(short.pending_message_ids[0], vec!["m1".to_string()]);
        assert!(short.pending_message_ids[1].is_empty());

        let mut long = HarnessSnapshot::initial(0, queued(&["only one"]));
        long.pending_message_ids = vec![vec!["m1".into()], vec!["stale".into()]];
        long.pending_envelope_ids = vec![Some(1), Some(2)];
        long.align_pending_side_arrays();
        assert_eq!(long.pending_message_ids, vec![vec!["m1".to_string()]]);
        assert_eq!(long.pending_envelope_ids, vec![Some(1)]);
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
    use crate::model::{HarnessInputPresentation, HarnessInputSegment};
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
            "pending_message_ids": [],
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
        assert_eq!(
            snapshot.issued_input_segments, None,
            "pre-#1270 snapshots have no structured input classification"
        );
    }

    #[test]
    fn issued_input_segments_round_trip_without_a_schema_bump() {
        let mut snapshot = HarnessSnapshot::initial(0, vec![]);
        let segments = vec![HarnessInputSegment {
            presentation: HarnessInputPresentation::SystemReportEdited,
            text: "report changed".into(),
        }];
        snapshot.issued_input_segments = Some(IssuedInputSegments {
            turn_id: "turn-structured".into(),
            segments: segments.clone(),
        });

        let value = serde_json::to_value(snapshot).expect("serialize snapshot");
        let recovered = HarnessSnapshot::from_value_strict(value);
        assert_eq!(
            recovered.issued_input_segments,
            Some(IssuedInputSegments {
                turn_id: "turn-structured".into(),
                segments,
            })
        );
    }
}
