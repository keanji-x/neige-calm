//! Persisted `PlannerHarness` state. Future schema bumps must migrate rows in the boot recovery
//! path before tasks are respawned; an unknown `schema_version` panics.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::Observation;
use crate::harness::queue::{QueueEntry, QueueEntryId};
use crate::harness::state::{HarnessState, IssuingKind};
use crate::harness::token_usage::TokenUsage;
use crate::model::HarnessInputSegment;
use crate::planner_attachments::bind::BoundAttachment;

pub use calm_types::harness::HarnessPhaseTag;

pub use calm_types::harness::{HARNESS_MODE, HARNESS_SNAPSHOT_SCHEMA_VERSION};

/// The segments of the turn an older binary had in flight when it stopped. Read on load and
/// never written (the type cannot be serialized): that turn has no projection row, so without
/// this its echo would be stored with no segments.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct IssuedInputSegments {
    pub turn_id: String,
    pub segments: Vec<HarnessInputSegment>,
}

/// The persisted half of a [`QueueEntry::User`], stored in an array parallel to `pending_queue`
/// because existing rows hold bare `Observation` values. The three identity fields are required,
/// which is safe only because [`deserialize_pending_entry_meta`] degrades an unparsable slot to
/// `None` (`LegacyUser`) instead of failing the snapshot; never remove that lenient decoder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueueEntryMeta {
    pub id: QueueEntryId,
    pub rev: u32,
    pub queued_at_ms: i64,
    /// The images this entry carries, with the absolute path each was bound to. The path is
    /// persisted rather than recomputed: it is decided once, before the entry exists.
    #[serde(default)]
    pub attachments: Vec<BoundAttachment>,
}

/// Decode one of the queue's parallel arrays ELEMENT BY ELEMENT: `from_value_strict` runs on the
/// boot path with no pre-validation, so a serde error anywhere is a dead harness. Each failed
/// element becomes that array's `Default` and the array keeps its length, so no position shifts.
fn lenient_parallel_array<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let raw = Vec::<Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .map(|value| serde_json::from_value::<T>(value).unwrap_or_default())
        .collect())
}

/// Read the `pending_entry_meta` array without ever failing the snapshot. A slot is dropped to
/// `None` (its entry reads back as `LegacyUser`) when it does not parse, when the id is empty,
/// or when the id repeats within one queue. Nothing here can promote a slot, only demote it.
fn deserialize_pending_entry_meta<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<Option<QueueEntryMeta>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<Value>::deserialize(deserializer)?;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    Ok(raw
        .into_iter()
        .map(|value| {
            let meta = serde_json::from_value::<QueueEntryMeta>(value).ok()?;
            if meta.id.as_str().is_empty() || !seen.insert(meta.id.as_str().to_string()) {
                return None;
            }
            Some(meta)
        })
        .collect())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HarnessSnapshot {
    pub schema_version: u32,
    pub mode: String,
    pub phase: HarnessPhaseTag,
    #[serde(default)]
    pub push_watermark: i64,
    /// The four arrays below are ONE value split across four keys for on-disk compatibility, and
    /// module-private so the split cannot be written apart. Serde still admits arrays of different
    /// lengths; `pending_entries()` pads the short sides with `None` rather than panicking.
    #[serde(default, deserialize_with = "lenient_parallel_array")]
    pending_queue: Vec<Option<Observation>>,
    #[serde(default, deserialize_with = "lenient_parallel_array")]
    pending_envelope_ids: Vec<Option<i64>>,
    /// Absent in every old row; an absent or `None` slot beside a `UserMessage` is read back as
    /// [`QueueEntry::LegacyUser`] and is never given an id.
    #[serde(default, deserialize_with = "deserialize_pending_entry_meta")]
    pending_entry_meta: Vec<Option<QueueEntryMeta>>,
    /// One set of message ids per `pending_queue` entry — "which message INSTANCES is this entry
    /// still holding", distinct from `QueueEntryId`. Identity only, not a state machine. A set
    /// because a fold unions two entries; empty until a transfer boundary mints one.
    #[serde(default, deserialize_with = "lenient_parallel_array")]
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
    /// Legacy key: still READ for the one turn an older binary can have left in flight, never
    /// written (`skip_serializing`, and the type derives no `Serialize`).
    #[serde(default, skip_serializing)]
    pub issued_input_segments: Option<IssuedInputSegments>,
    /// The key under which the batch at the head of the queue is, or is about to be, projected onto
    /// the transcript table. Written by `maybe_issue_turn` before the drain and cleared once the turn
    /// is out; once decided, the slot outranks whatever the queue holds on a re-drain.
    #[serde(default)]
    pub projection_client_id: Option<QueueEntryId>,
    #[serde(default)]
    pub wedged_reason: Option<String>,
    /// Latest `thread/tokenUsage/updated` reading. Additive and defaulted, no `schema_version` bump:
    /// an old binary ignores the unknown key and loses only a value re-pushed on the next response,
    /// whereas a bump would turn a lossless rollback into a boot panic.
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
            projection_client_id: None,
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
            projection_client_id: None,
            // Set by `snapshot_for` from `Inner`; `from_state` sees only `HarnessState`.
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

    /// Zip the four stored arrays into one fused view; shorter sides are padded with `None`.
    /// `UserMessage` + `Some` meta → `User`; `UserMessage` + `None` → `LegacyUser`; anything else →
    /// `System` (a meta slot there is ignored, never honoured).
    pub fn pending_entries(&self) -> Vec<QueueEntry> {
        self.pending_queue
            .iter()
            .enumerate()
            .filter_map(|(index, observation)| {
                // A hole is a position whose observation this build could not read; the whole position goes,
                // side slots included, so nothing below is paired with a neighbour's identity.
                let observation = observation.as_ref()?;
                let envelope_id = self.pending_envelope_ids.get(index).copied().flatten();
                let meta = self.pending_entry_meta.get(index).and_then(Option::as_ref);
                // Same padding rule as the other side arrays: an absent slot reads as "no instance identity
                // yet", never as somebody else's.
                let message_ids = self
                    .pending_message_ids
                    .get(index)
                    .cloned()
                    .unwrap_or_default();
                Some(match (observation, meta) {
                    (Observation::UserMessage { text }, Some(meta)) => QueueEntry::User {
                        id: meta.id.clone(),
                        text: text.clone(),
                        rev: meta.rev,
                        queued_at_ms: meta.queued_at_ms,
                        envelope_id,
                        message_ids,
                        attachments: meta.attachments.clone(),
                    },
                    (Observation::UserMessage { text }, None) => {
                        QueueEntry::legacy_user(text.clone(), envelope_id, message_ids)
                    }
                    (other, _) => QueueEntry::System {
                        observation: other.clone(),
                        envelope_id,
                        message_ids,
                    },
                })
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
                    attachments,
                    ..
                } => Some(QueueEntryMeta {
                    id: id.clone(),
                    rev: *rev,
                    queued_at_ms: *queued_at_ms,
                    attachments: attachments.clone(),
                }),
                // A legacy entry is written back exactly as it was read: text present, meta slot empty.
                QueueEntry::LegacyUser { .. } | QueueEntry::System { .. } => None,
            });
            pending_queue.push(Some(entry.observation()));
        }
        self.pending_queue = pending_queue;
        self.pending_envelope_ids = pending_envelope_ids;
        self.pending_entry_meta = pending_entry_meta;
        self.pending_message_ids = pending_message_ids;
    }

    /// Read-only view of the observations; holes are skipped exactly as `pending_entries` skips them.
    pub fn pending_observations(&self) -> Vec<Observation> {
        self.pending_queue.iter().flatten().cloned().collect()
    }

    pub fn pending_len(&self) -> usize {
        self.pending_queue.iter().flatten().count()
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
            .map(|text| QueueEntry::user_message((*text).to_string(), None, Vec::new()))
            .collect()
    }

    /// A snapshot written before `pending_message_ids` existed decodes with an EMPTY outer vec
    /// against a non-empty queue; the padding must happen on the READ, not only at the decoder.
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

    /// A deliberately mismatched snapshot must be CORRECTED on the way out, in both directions.
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

        // Re-persisting through the single write point drops the orphan.
        let kept = long.pending_entries();
        long.set_pending_entries(kept);
        assert_eq!(long.pending_message_ids, vec![vec!["m1".to_string()]]);
        assert_eq!(long.pending_envelope_ids, vec![Some(1)]);
    }
}

impl HarnessSnapshot {
    pub(crate) fn parse_known(value: Value) -> Option<Self> {
        let snapshot: Self = serde_json::from_value(value).ok()?;
        (snapshot.schema_version == HARNESS_SNAPSHOT_SCHEMA_VERSION
            && snapshot.mode == HARNESS_MODE)
            .then_some(snapshot)
    }
}

pub fn is_harness_snapshot_value(value: &Value) -> bool {
    HarnessSnapshot::parse_known(value.clone()).is_some()
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

    /// A snapshot as a pre-`token_usage` binary wrote it: no key at all. Has to be a literal —
    /// every other `from_value_strict` call site feeds JSON a current binary just serialized.
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
    }

    /// The snapshot layer never silently gives an old queue entry a new id. Has to be a hand-written
    /// literal: every other call site feeds JSON with `pending_entry_meta` present.
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

        // Re-persisting does not repair it: a LegacyUser is written back legacy.
        let round_tripped = HarnessSnapshot::from_value_strict(
            serde_json::to_value(&snapshot).expect("serialize snapshot"),
        );
        assert_eq!(
            round_tripped.pending_entries(),
            entries,
            "a legacy entry stays legacy across a persist/reload cycle"
        );
    }

    /// One list in, every parallel array out, always equal length.
    #[test]
    fn set_pending_entries_writes_every_parallel_array_in_step() {
        let user = QueueEntry::user_message("hello".into(), Some(9), Vec::new());
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
        let message_ids = value["pending_message_ids"].as_array().expect("array");
        assert_eq!(message_ids.len(), 3, "the message-id array is never short");
        assert_eq!(
            message_ids[0].as_array().expect("array").len(),
            1,
            "#1449 — a fresh user entry is minted with a transfer identity"
        );
        assert_eq!(message_ids[1], json!([]), "system entries hold no instance");
        assert_eq!(
            message_ids[2],
            json!([]),
            "and a legacy entry holds none until a transfer boundary mints one"
        );

        let recovered = HarnessSnapshot::from_value_strict(value);
        assert_eq!(
            recovered.pending_entries()[0].id(),
            Some(&user_id),
            "an id survives the round trip unchanged"
        );
        assert_eq!(recovered.pending_entries()[2], legacy);
    }

    /// Padding rather than panicking is the fail-open direction on purpose — a panic here is a dead harness.
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

    /// A legacy `issued_input_segments` key is READ (the echo of that turn needs those segments)
    /// and never WRITTEN.
    #[test]
    fn a_pre_p2_issued_input_segments_key_is_read_on_load_and_never_written() {
        let mut value =
            serde_json::to_value(HarnessSnapshot::initial(7, vec![])).expect("serialize snapshot");
        assert_eq!(
            value.get("issued_input_segments"),
            None,
            "this binary writes no such key"
        );
        value["issued_input_segments"] = serde_json::json!({
            "turn_id": "turn-structured",
            "segments": [{
                "presentation": "system_report_edited",
                "text": "report changed",
                "attachments": [],
            }],
        });
        let recovered = HarnessSnapshot::from_value_strict(value);
        assert_eq!(recovered.push_watermark, 7);
        assert!(recovered.pending_entries().is_empty());
        let legacy = recovered
            .issued_input_segments
            .as_ref()
            .expect("the pre-P2 key is read, not dropped");
        assert_eq!(legacy.turn_id, "turn-structured");
        assert_eq!(
            legacy.segments,
            vec![HarnessInputSegment {
                presentation: crate::model::HarnessInputPresentation::SystemReportEdited,
                text: "report changed".into(),
                attachments: Vec::new(),
            }]
        );
        let rewritten = serde_json::to_value(&recovered).expect("serialize snapshot");
        assert_eq!(
            rewritten.get("issued_input_segments"),
            None,
            "read once at load; the next write drops the key"
        );
    }

    /// A row from a later binary carries unknown keys; it must boot and ignore them. Rests on the
    /// absence of `#[serde(deny_unknown_fields)]`.
    #[test]
    fn an_unknown_key_from_a_future_binary_is_ignored_not_rejected() {
        let mut row = serde_json::to_value(HarnessSnapshot::initial(
            0,
            vec![QueueEntry::user_message("hello".into(), None, Vec::new())],
        ))
        .expect("serialize snapshot");
        row["pending_entry_meta_v2"] = json!([{"steer_state": "queued"}]);
        row["something_a_later_slice_added"] = json!({"nested": [1, 2, 3]});

        assert!(
            is_harness_snapshot_value(&row),
            "an unknown key must not make the row unrecognisable as a snapshot"
        );
        let recovered = HarnessSnapshot::from_value_strict(row);
        assert_eq!(
            recovered.pending_entries().len(),
            1,
            "and the entries this build DOES understand still arrive"
        );
    }

    /// A meta slot this build cannot parse degrades to `LegacyUser`; it never panics.
    #[test]
    fn an_unreadable_element_in_any_parallel_array_degrades_instead_of_panicking() {
        // (array under test, the two elements — one good, one this build cannot read)
        let cases: [(&str, Value); 3] = [
            // A future `Observation` variant, as this build sees it.
            (
                "pending_queue",
                json!([
                    {"type": "user_message", "text": "first"},
                    {"type": "from_a_later_slice", "payload": 1}
                ]),
            ),
            // A hand-edited row, or a wire shape that grew.
            ("pending_envelope_ids", json!([7, "not an integer"])),
            // The array the review reported.
            ("pending_message_ids", json!([["m1"], "oops"])),
        ];

        for (array, planted) in cases {
            let mut row = json!({
                "schema_version": HARNESS_SNAPSHOT_SCHEMA_VERSION,
                "mode": HARNESS_MODE,
                "phase": "idle",
                "pending_queue": [
                    {"type": "user_message", "text": "first"},
                    {"type": "user_message", "text": "second"}
                ],
                "pending_envelope_ids": [7, 8],
                "pending_entry_meta": [
                    {"id": "id-first", "rev": 0, "queued_at_ms": 5},
                    {"id": "id-second", "rev": 0, "queued_at_ms": 6}
                ],
                "pending_message_ids": [["m1"], ["m2"]]
            });
            row[array] = planted;

            // Would have panicked; `expect` on the boot path is a dead card.
            let snapshot = HarnessSnapshot::from_value_strict(row);
            let entries = snapshot.pending_entries();

            // The good element at index 0 keeps everything that was ITS own.
            assert_eq!(
                entries[0].observation(),
                Observation::UserMessage {
                    text: "first".into()
                },
                "{array}: the readable entry must survive intact"
            );
            assert_eq!(
                entries[0].id().map(QueueEntryId::as_str),
                Some("id-first"),
                "{array}: …still holding its OWN queue id"
            );
            assert_eq!(
                entries[0].envelope_id(),
                Some(7),
                "{array}: …its own envelope id"
            );
            assert_eq!(
                entries[0].message_ids(),
                ["m1".to_string()],
                "{array}: …and its own transfer identity, not its neighbour's"
            );

            match array {
                // A hole removes the POSITION; anything else would be a fabricated observation.
                "pending_queue" => assert_eq!(
                    entries.len(),
                    1,
                    "an observation this build cannot read is not a queue entry"
                ),
                // The other two keep the entry and lose only that one position's value.
                "pending_envelope_ids" => {
                    assert_eq!(entries.len(), 2);
                    assert_eq!(entries[1].envelope_id(), None, "{array}: degraded slot");
                    assert_eq!(
                        entries[1].id().map(QueueEntryId::as_str),
                        Some("id-second"),
                        "{array}: and only that slot"
                    );
                }
                "pending_message_ids" => {
                    assert_eq!(entries.len(), 2);
                    assert!(
                        entries[1].message_ids().is_empty(),
                        "{array}: degraded slot says `no transfer identity yet`"
                    );
                    assert_eq!(
                        entries[1].id().map(QueueEntryId::as_str),
                        Some("id-second"),
                        "{array}: and only that slot"
                    );
                }
                other => panic!("unhandled case {other}"),
            }
        }
    }

    /// Also the sentinel for the decoder itself: deleting `deserialize_pending_entry_meta` reddens it.
    #[test]
    fn a_meta_slot_a_future_field_broke_degrades_to_legacy_instead_of_panicking() {
        let row = json!({
            "schema_version": HARNESS_SNAPSHOT_SCHEMA_VERSION,
            "mode": HARNESS_MODE,
            "phase": "idle",
            "pending_queue": [
                {"type": "user_message", "text": "written by PR1"},
                {"type": "user_message", "text": "also written by PR1"}
            ],
            "pending_envelope_ids": [null, null],
            // As a future binary that made `queued_at_ms` required would see an older row.
            "pending_entry_meta": [
                {"id": "kept", "rev": 0, "queued_at_ms": 5},
                {"id": "broken", "rev": 0}
            ]
        });

        let entries = HarnessSnapshot::from_value_strict(row).pending_entries();

        assert_eq!(
            entries[0].id().map(QueueEntryId::as_str),
            Some("kept"),
            "a slot this build understands is untouched"
        );
        assert_eq!(
            entries[1],
            QueueEntry::legacy_user("also written by PR1".into(), None, Vec::new()),
            "an unparseable slot degrades to LegacyUser rather than failing the boot"
        );
    }

    /// The two identities serde could smuggle past the minting path (empty id, duplicate id) are
    /// refused at the read boundary; neither is reachable from Rust.
    #[test]
    fn an_empty_or_duplicated_entry_id_is_refused_rather_than_addressed() {
        let row = json!({
            "schema_version": HARNESS_SNAPSHOT_SCHEMA_VERSION,
            "mode": HARNESS_MODE,
            "phase": "idle",
            "pending_queue": [
                {"type": "user_message", "text": "empty id"},
                {"type": "user_message", "text": "first holder of dup"},
                {"type": "user_message", "text": "second holder of dup"}
            ],
            "pending_envelope_ids": [null, null, null],
            "pending_entry_meta": [
                {"id": "", "rev": 0, "queued_at_ms": 1},
                {"id": "dup", "rev": 0, "queued_at_ms": 2},
                {"id": "dup", "rev": 0, "queued_at_ms": 3}
            ]
        });

        let entries = HarnessSnapshot::from_value_strict(row).pending_entries();

        assert_eq!(entries[0].id(), None, "`\"\"` is not an address");
        assert_eq!(
            entries[1].id().map(QueueEntryId::as_str),
            Some("dup"),
            "the first holder keeps the id"
        );
        assert_eq!(
            entries[2].id(),
            None,
            "and the second is demoted, so no id ever names two entries"
        );
        assert!(
            entries.iter().all(QueueEntry::is_user_authored),
            "demotion never hides a message from the planner or from the overflow count"
        );
    }
}
