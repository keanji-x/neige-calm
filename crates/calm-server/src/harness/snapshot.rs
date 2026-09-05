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
use crate::harness::queue::{QueueEntry, QueueEntryId};
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

/// #1505 PR1 — the persisted half of a [`QueueEntry::User`].
///
/// Stored in a third array parallel to `pending_queue`, rather than by turning
/// `pending_queue` into an array of objects. That is not a style choice: the
/// existing rows on disk hold bare `Observation` values, and `from_value_strict`
/// has no pre-validation in boot recovery, so an incompatible element shape
/// would panic every live harness on upgrade.
///
/// A `None` slot beside a `UserMessage` is the whole of what makes a
/// [`QueueEntry::LegacyUser`]; see that variant for why it is never repaired.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueueEntryMeta {
    pub id: QueueEntryId,
    pub rev: u32,
    pub queued_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HarnessSnapshot {
    pub schema_version: u32,
    pub mode: String,
    pub phase: HarnessPhaseTag,
    #[serde(default)]
    pub push_watermark: i64,
    /// The four arrays below are ONE value split across four keys for
    /// on-disk compatibility, and they are module-private so that the split
    /// cannot be written apart. Every reader goes through
    /// [`HarnessSnapshot::pending_entries`], every writer through
    /// [`HarnessSnapshot::set_pending_entries`].
    ///
    /// The visibility argument covers Rust writers in this crate only. It does
    /// NOT cover serde: a hand-written JSON row with three arrays of different
    /// lengths still deserializes, and `pending_entries()` pads the short sides
    /// with `None` rather than panicking — deliberately, because panicking here
    /// is a boot failure for a live harness (see `token_usage` below).
    #[serde(default)]
    pending_queue: Vec<Observation>,
    #[serde(default)]
    pending_envelope_ids: Vec<Option<i64>>,
    /// #1505 PR1. Absent in every row written before this slice; an absent or
    /// `None` slot beside a `UserMessage` is read back as
    /// [`QueueEntry::LegacyUser`] and is never given an id.
    ///
    /// No `HARNESS_SNAPSHOT_SCHEMA_VERSION` bump, for exactly the reasons
    /// spelled out on `token_usage` below: no `deny_unknown_fields` anywhere in
    /// this type, `#[serde(default)]` here, and `assert_known_schema` compares
    /// only the integer — so a new binary reads an old row (this array is
    /// empty, every user entry is legacy) and an old binary reads a new row
    /// (the key is ignored, entries lose their ids and are re-read as legacy on
    /// the way back). Bumping would turn that lossless rollback into a boot
    /// panic. Pinned by
    /// `a_pre_1505_snapshot_without_pending_entry_meta_yields_legacy_entries`.
    #[serde(default)]
    pending_entry_meta: Vec<Option<QueueEntryMeta>>,
    /// #1449 — one set of message ids per `pending_queue` entry, so two copies
    /// of the same sentence can be told apart wherever they meet.
    ///
    /// **A second identity, and deliberately not `pending_entry_meta`'s.**
    /// `QueueEntryId` answers "which entry is the client addressing", is minted
    /// once at enqueue, survives a fold on the SURVIVOR only, and is never
    /// given to a [`QueueEntry::LegacyUser`]. These ids answer "which message
    /// INSTANCES is this entry still holding", are minted wherever an instance
    /// crosses a transfer boundary without one (legacy entries included), and a
    /// fold unions them. Collapsing the two would either make legacy entries
    /// addressable or make a fold drop an instance the give-back must move
    /// back; #1449 round 6 and #1505 GAP-B are each pinned against one of those.
    ///
    /// **Identity only. Not a state machine.** These ids answer exactly one
    /// question — "is this the same instance I moved?" — and they must not grow
    /// a disposition, an accepted/canceled terminal state, or any other
    /// semantics. A queue transfer that cannot tell instances apart degrades
    /// into matching on message text, which is forgeable (two identical
    /// sentences) and is the shape this repository has already been hurt by.
    ///
    /// Empty for every non-`UserMessage` entry, and for an entry enqueued
    /// before this field existed — but only until that entry moves: the harvest
    /// and the inherit mint an id for any `UserMessage` they carry that has
    /// none, in the transaction that moves it, so a moved instance is always
    /// identifiable. Minting there rather than at load keeps the id stable
    /// across reads.
    #[serde(default)]
    pending_message_ids: Vec<Vec<String>>,
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

impl HarnessSnapshot {
    pub fn initial(push_watermark: i64, entries: Vec<QueueEntry>) -> Self {
        let mut snapshot = Self {
            schema_version: HARNESS_SNAPSHOT_SCHEMA_VERSION,
            mode: HARNESS_MODE.to_string(),
            phase: HarnessPhaseTag::PendingThreadStart,
            push_watermark,
            pending_queue: Vec::new(),
            pending_envelope_ids: Vec::new(),
            pending_entry_meta: Vec::new(),
            pending_message_ids: Vec::new(),
            last_thread_id: None,
            last_turn_id: None,
            last_report_body_sha256: None,
            last_seen_head: None,
            issued_turn_head: None,
            issued_input_segments: None,
            wedged_reason: None,
            token_usage: None,
        };
        snapshot.set_pending_entries(entries);
        snapshot
    }

    pub fn from_state(
        state: &HarnessState,
        push_watermark: i64,
        entries: Vec<QueueEntry>,
        last_thread_id: Option<String>,
        last_turn_id: Option<String>,
        last_report_body_sha256: Option<String>,
    ) -> Self {
        let phase = HarnessPhaseTag::from(state);
        let wedged_reason = match state {
            HarnessState::Wedged { reason, .. } => Some(reason.clone()),
            _ => None,
        };
        let mut snapshot = Self {
            schema_version: HARNESS_SNAPSHOT_SCHEMA_VERSION,
            mode: HARNESS_MODE.to_string(),
            phase,
            push_watermark,
            pending_queue: Vec::new(),
            pending_envelope_ids: Vec::new(),
            pending_entry_meta: Vec::new(),
            pending_message_ids: Vec::new(),
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
        };
        snapshot.set_pending_entries(entries);
        snapshot
    }

    pub fn from_value_strict(value: Value) -> Self {
        let snapshot: Self =
            serde_json::from_value(value).expect("deserialize PlannerHarness snapshot");
        snapshot.assert_known_schema();
        snapshot
    }

    /// Zip the four stored arrays into one fused view.
    ///
    /// Shorter sides are padded with `None`, which is where the old
    /// `align_pending_envelope_ids` pass went. The variant is decided by the
    /// observation together with its meta slot, and the mapping is total:
    ///
    /// | observation | meta slot | entry |
    /// |---|---|---|
    /// | `UserMessage` | `Some` | [`QueueEntry::User`] |
    /// | `UserMessage` | `None` | [`QueueEntry::LegacyUser`] |
    /// | anything else | ignored | [`QueueEntry::System`] |
    ///
    /// The third row ignores rather than honours a meta slot: no writer in this
    /// crate can produce one (`set_pending_entries` writes `None` for every
    /// non-user entry), and honouring a hand-written one would be the start of
    /// an addressable system observation, which the enum exists to forbid.
    pub fn pending_entries(&self) -> Vec<QueueEntry> {
        self.pending_queue
            .iter()
            .enumerate()
            .map(|(index, observation)| {
                let envelope_id = self.pending_envelope_ids.get(index).copied().flatten();
                let meta = self.pending_entry_meta.get(index).and_then(Option::as_ref);
                // #1449 — the same padding rule as the other side arrays: a
                // row written before `pending_message_ids` existed decodes to
                // an EMPTY outer vec against an N-entry queue, and an absent
                // slot must read as "no instance identity yet", never as
                // somebody else's.
                let message_ids = self
                    .pending_message_ids
                    .get(index)
                    .cloned()
                    .unwrap_or_default();
                match (observation, meta) {
                    (Observation::UserMessage { text }, Some(meta)) => QueueEntry::User {
                        id: meta.id.clone(),
                        text: text.clone(),
                        rev: meta.rev,
                        queued_at_ms: meta.queued_at_ms,
                        envelope_id,
                        message_ids,
                    },
                    (Observation::UserMessage { text }, None) => {
                        QueueEntry::legacy_user(text.clone(), envelope_id, message_ids)
                    }
                    (other, _) => QueueEntry::System {
                        observation: other.clone(),
                        envelope_id,
                        message_ids,
                    },
                }
            })
            .collect()
    }

    /// The single write point for the pending queue. All four arrays are
    /// rebuilt together, so they are equal-length by construction.
    pub fn set_pending_entries(&mut self, entries: Vec<QueueEntry>) {
        let len = entries.len();
        let mut pending_queue = Vec::with_capacity(len);
        let mut pending_envelope_ids = Vec::with_capacity(len);
        let mut pending_entry_meta = Vec::with_capacity(len);
        let mut pending_message_ids = Vec::with_capacity(len);
        for entry in entries {
            pending_envelope_ids.push(entry.envelope_id());
            pending_message_ids.push(entry.message_ids().to_vec());
            pending_entry_meta.push(match &entry {
                QueueEntry::User {
                    id,
                    rev,
                    queued_at_ms,
                    ..
                } => Some(QueueEntryMeta {
                    id: id.clone(),
                    rev: *rev,
                    queued_at_ms: *queued_at_ms,
                }),
                // A legacy entry is written back exactly as it was read: text
                // present, meta slot empty. That is what keeps it legacy across
                // any number of restarts, and what keeps GAP-B true.
                QueueEntry::LegacyUser { .. } | QueueEntry::System { .. } => None,
            });
            pending_queue.push(entry.observation());
        }
        self.pending_queue = pending_queue;
        self.pending_envelope_ids = pending_envelope_ids;
        self.pending_entry_meta = pending_entry_meta;
        self.pending_message_ids = pending_message_ids;
    }

    /// Read-only convenience for callers that only care about the observations.
    pub fn pending_observations(&self) -> Vec<Observation> {
        self.pending_queue.clone()
    }

    pub fn pending_len(&self) -> usize {
        self.pending_queue.len()
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
}

#[cfg(test)]
mod pending_side_array_tests {
    use super::*;
    use crate::harness::queue::QueueEntry;

    fn queued(texts: &[&str]) -> Vec<QueueEntry> {
        texts
            .iter()
            .map(|text| QueueEntry::user_message((*text).to_string(), None))
            .collect()
    }

    /// #1449 — a snapshot written before `pending_message_ids` existed decodes
    /// with an EMPTY outer vec against a non-empty queue.
    ///
    /// That length mismatch is worse than having no ids at all: any code that
    /// pairs the arrays by index would attribute an id to the wrong entry, and
    /// the give-back would then move the wrong sentence. #1505 PR1 moved the
    /// padding out of a separate aligner and into `pending_entries`, so the
    /// mismatch cannot leave the READ — which is stronger than aligning at the
    /// decoder only, because a snapshot built in memory gets it too.
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
        let entries = snapshot.pending_entries();
        assert_eq!(entries.len(), 3);
        assert!(
            entries.iter().all(|entry| entry.message_ids().is_empty()),
            "every entry must say `no identity`, not borrow somebody else's"
        );
    }

    /// The counter-fixture: a deliberately mismatched snapshot must be
    /// CORRECTED on the way out, not carried through.
    ///
    /// Both directions: a short array leaves entries with no id, a long one
    /// pairs ids with entries that do not exist, and either one is a
    /// mis-attribution the give-back would act on.
    #[test]
    fn alignment_corrects_both_a_short_and_a_long_id_array() {
        let mut short = HarnessSnapshot::initial(0, queued(&["one", "two", "three"]));
        short.pending_message_ids = vec![vec!["m1".into()]];
        short.pending_envelope_ids = vec![Some(1)];
        let entries = short.pending_entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].message_ids(), ["m1".to_string()]);
        assert_eq!(entries[0].envelope_id(), Some(1));
        assert!(entries[1].message_ids().is_empty());
        assert_eq!(entries[1].envelope_id(), None);

        let mut long = HarnessSnapshot::initial(0, queued(&["only one"]));
        long.pending_message_ids = vec![vec!["m1".into()], vec!["stale".into()]];
        long.pending_envelope_ids = vec![Some(1), Some(2)];
        let entries = long.pending_entries();
        assert_eq!(
            entries.len(),
            1,
            "the queue decides the length, not a side array"
        );
        assert_eq!(entries[0].message_ids(), ["m1".to_string()]);

        // And the correction is not merely a read-side view: re-persisting
        // through the single write point drops the orphan.
        let kept = long.pending_entries();
        long.set_pending_entries(kept);
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

    /// #1505 PR1 §11.1 #3 — the ONE test that pins "PR1 never silently gives
    /// an old queue entry a new id".
    ///
    /// It has to be a hand-written literal. Every other `from_value_strict`
    /// call site in the suite feeds JSON that a *current* binary just
    /// serialized, in which `pending_entry_meta` is always present and
    /// populated, so the absent-key path is otherwise never exercised.
    ///
    /// Mutation-verified (`MUTATION-1505-PR1`): changing the `(UserMessage,
    /// None)` arm of `pending_entries` from `legacy_user(..)` to
    /// `QueueEntry::user_message(..)` — i.e. minting instead of degrading —
    /// reddens this test and only this test.
    #[test]
    fn a_pre_1505_snapshot_without_pending_entry_meta_yields_legacy_entries() {
        let pre_1505 = json!({
            "schema_version": HARNESS_SNAPSHOT_SCHEMA_VERSION,
            "mode": HARNESS_MODE,
            "phase": "turn_running",
            "push_watermark": 7,
            "pending_queue": [
                {"type": "user_message", "text": "written before PR1"},
                {"type": "track_goal", "text": "ship the thing"}
            ],
            "pending_envelope_ids": [null, 42],
            "last_thread_id": "thread-pre-1505",
            "last_turn_id": null,
            "last_report_body_sha256": null,
            "last_seen_head": null,
            "issued_turn_head": null,
            "wedged_reason": null
        });
        assert!(
            pre_1505.get("pending_entry_meta").is_none(),
            "the point of this literal is the ABSENT key; do not add it"
        );
        assert!(
            is_harness_snapshot_value(&pre_1505),
            "a pre-#1505 row must still be recognised as a harness snapshot — \
             `routes::cards::get_planner_run` uses this to decide dormant-vs-live"
        );

        let snapshot = HarnessSnapshot::from_value_strict(pre_1505);
        let entries = snapshot.pending_entries();
        assert_eq!(entries.len(), 2);

        assert_eq!(
            entries[0],
            QueueEntry::legacy_user("written before PR1".into(), None, Vec::new()),
            "an old user entry is degraded to LegacyUser, NOT minted a fresh id"
        );
        assert_eq!(
            entries[0].id(),
            None,
            "and it has no id at all — there is no field to put one in"
        );
        assert!(
            entries[0].user_view().is_none(),
            "so it cannot reach the addressable `pending` page"
        );
        assert!(
            entries[0].is_user_authored(),
            "but it still counts toward `pending_overflow`"
        );

        assert_eq!(
            entries[1],
            QueueEntry::system(
                Observation::TrackGoal {
                    text: "ship the thing".into()
                },
                Some(42)
            )
            .expect("a track goal is a system entry"),
            "envelope ids still line up positionally across the gap"
        );

        // Re-persisting does not repair it: a LegacyUser written back is
        // written back legacy, so it reads back legacy on the next boot too.
        let round_tripped = HarnessSnapshot::from_value_strict(
            serde_json::to_value(&snapshot).expect("serialize snapshot"),
        );
        assert_eq!(
            round_tripped.pending_entries(),
            entries,
            "a legacy entry stays legacy across a persist/reload cycle"
        );
    }

    /// §1.5 — the fused write point is the reason a partial update cannot be
    /// expressed any more. Three arrays in, three arrays out, always equal
    /// length, with the meta slot populated for exactly the addressable
    /// entries.
    #[test]
    fn set_pending_entries_writes_all_three_arrays_in_step() {
        let user = QueueEntry::user_message("hello".into(), Some(9));
        let user_id = user.id().cloned().expect("a fresh user entry has an id");
        let system = QueueEntry::system(
            Observation::TrackGoal {
                text: "goal".into(),
            },
            None,
        )
        .expect("system entry");
        let legacy = QueueEntry::legacy_user("old".into(), None, Vec::new());

        let snapshot = HarnessSnapshot::initial(0, vec![user, system, legacy.clone()]);
        let value = serde_json::to_value(&snapshot).expect("serialize snapshot");

        assert_eq!(value["pending_queue"].as_array().expect("array").len(), 3);
        assert_eq!(
            value["pending_envelope_ids"],
            json!([9, null, null]),
            "envelope ids ride the same index as their observation"
        );
        let meta = value["pending_entry_meta"].as_array().expect("array");
        assert_eq!(meta.len(), 3, "the meta array is never short");
        assert_eq!(meta[0]["id"], json!(user_id.as_str()));
        assert_eq!(meta[0]["rev"], json!(0));
        assert!(meta[1].is_null(), "system entries carry no meta");
        assert!(meta[2].is_null(), "legacy entries carry no meta");

        let recovered = HarnessSnapshot::from_value_strict(value);
        assert_eq!(
            recovered.pending_entries()[0].id(),
            Some(&user_id),
            "an id survives the round trip unchanged"
        );
        assert_eq!(recovered.pending_entries()[2], legacy);
    }

    /// A hand-written row with mismatched array lengths still loads: the short
    /// sides pad with `None`. This is the serde hole the visibility argument
    /// explicitly does not close, and padding rather than panicking is the
    /// fail-open direction on purpose — a panic here is a dead harness.
    #[test]
    fn mismatched_parallel_arrays_pad_instead_of_panicking() {
        let ragged = json!({
            "schema_version": HARNESS_SNAPSHOT_SCHEMA_VERSION,
            "mode": HARNESS_MODE,
            "phase": "idle",
            "push_watermark": 0,
            "pending_queue": [
                {"type": "user_message", "text": "one"},
                {"type": "user_message", "text": "two"}
            ],
            "pending_envelope_ids": [5],
            "pending_entry_meta": []
        });
        let entries = HarnessSnapshot::from_value_strict(ragged).pending_entries();
        assert_eq!(
            entries,
            vec![
                QueueEntry::legacy_user("one".into(), Some(5), Vec::new()),
                QueueEntry::legacy_user("two".into(), None, Vec::new()),
            ]
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
