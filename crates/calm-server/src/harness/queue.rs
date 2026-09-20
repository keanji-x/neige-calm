//! Identity for entries sitting in the harness pending queue. Address-by-id is only
//! unambiguous while one id names one entry; `apply_mutation` refuses on more than one match.

use std::collections::{HashSet, VecDeque};

use crate::error::{CalmError, Result};
use crate::event::HarnessQueueChange;
use crate::harness::observation::Observation;
use crate::ids::CardId;
use crate::model::{HarnessInputSegment, new_id, now_ms};
use crate::planner_attachments::bind::{BoundAttachment, MAX_ATTACHMENTS_PER_MESSAGE};

/// Stable identity for one addressable user entry in the pending queue.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct QueueEntryId(String);

impl QueueEntryId {
    /// Mint a fresh id; production reaches this only through [`QueueEntry::user_message`].
    pub(crate) fn mint() -> Self {
        Self(new_id())
    }

    /// Adopt an id that arrived from a client, verbatim; an id from a URL is a lookup key, never a claim.
    pub fn from_wire(id: String) -> Self {
        Self(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for QueueEntryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One entry in the harness pending queue. `LegacyUser` is a `UserMessage` read back from a
/// snapshot whose `pending_entry_meta` slot is `None`; it has no `QueueEntryId` field at all and
/// is never repaired in place — it gains an id only when the harvest rebuilds it as `User`.
#[derive(Clone, Debug, PartialEq)]
pub enum QueueEntry {
    User {
        id: QueueEntryId,
        text: String,
        /// CAS token. Incremented every time the text is rewritten (folding included) and once more
        /// when the entry is restored after a client was told it had left.
        rev: u32,
        /// Wall-clock ms at which this entry entered the queue.
        queued_at_ms: i64,
        envelope_id: Option<i64>,
        /// Transfer identity. See [`QueueEntry::message_ids`].
        message_ids: Vec<String>,
        /// Images this message carries, already bound on the REST side, so drain, issue and
        /// re-buffer are pure in-memory work.
        attachments: Vec<BoundAttachment>,
    },
    LegacyUser {
        text: String,
        envelope_id: Option<i64>,
        /// Transfer identity. Empty until this entry crosses a transfer boundary, which mints one
        /// even though a legacy entry never gains a [`QueueEntryId`].
        message_ids: Vec<String>,
    },
    System {
        observation: Observation,
        envelope_id: Option<i64>,
        /// Always empty: only user-authored instances are ever moved back.
        message_ids: Vec<String>,
    },
}

/// Borrowed view of the addressable half of a [`QueueEntry::User`].
pub struct UserEntryView<'a> {
    pub id: &'a QueueEntryId,
    pub text: &'a str,
    pub rev: u32,
    pub queued_at_ms: i64,
    pub attachments: &'a [BoundAttachment],
}

impl QueueEntry {
    /// The entry is re-entering the queue after a client was told it had left; the bump lets the
    /// client tell the page after the restore from the page before. Only the completion sweep calls
    /// this — a steer codex refused restores WITHOUT a bump, or the client's retry would go `stale`.
    pub fn bump_rev_for_restore(&mut self) {
        if let Self::User { rev, .. } = self {
            *rev = rev.saturating_add(1);
        }
    }

    /// The one place a [`QueueEntryId`] is minted. `attachments` is required so a test cannot
    /// seed a shape production cannot produce.
    pub fn user_message(
        text: String,
        envelope_id: Option<i64>,
        attachments: Vec<BoundAttachment>,
    ) -> Self {
        Self::User {
            id: QueueEntryId::mint(),
            text,
            rev: 0,
            queued_at_ms: now_ms(),
            envelope_id,
            attachments,
            // The transfer identity is minted together with the `QueueEntryId`, and they are still two ids.
            message_ids: vec![new_id()],
        }
    }

    /// A user message arriving in a successor's queue by MOVE. It keeps the transfer identity it
    /// was moved with and the `QueueEntryId` the mover had (a sentence stays addressable under the
    /// SAME id). Attachments are NOT carried: their bytes live under the source card.
    pub fn user_message_moved(
        text: String,
        message_ids: Vec<String>,
        entry_id: Option<QueueEntryId>,
    ) -> Self {
        let mut entry = Self::user_message(text, None, Vec::new());
        if !message_ids.is_empty() {
            *entry.message_ids_mut() = message_ids;
        }
        if let Some(id) = entry_id
            && let Self::User { id: slot, .. } = &mut entry
        {
            *slot = id;
        }
        entry
    }

    /// Wrap a dispatcher observation. A `UserMessage` is refused — a runtime fail-closed guard
    /// live in release builds.
    pub fn system(observation: Observation, envelope_id: Option<i64>) -> Result<Self> {
        if matches!(observation, Observation::UserMessage { .. }) {
            return Err(CalmError::Internal(
                "QueueEntry::system refuses Observation::UserMessage; \
                 mint user input through QueueEntry::user_message"
                    .into(),
            ));
        }
        Ok(Self::System {
            observation,
            envelope_id,
            message_ids: Vec::new(),
        })
    }

    /// Fixtures-only bulk wrapper for seeding a snapshot from bare observations, dispatching on
    /// the variant exactly as the ingress does.
    #[cfg(feature = "fixtures")]
    pub fn entries_from_observations_for_test(observations: Vec<Observation>) -> Vec<Self> {
        observations
            .into_iter()
            .map(|observation| match observation {
                Observation::UserMessage { text } => Self::user_message(text, None, Vec::new()),
                other => Self::system(other, None)
                    .expect("a non-user observation wraps as a system entry"),
            })
            .collect()
    }

    /// Only for snapshot deserialization normalization.
    pub(in crate::harness) fn legacy_user(
        text: String,
        envelope_id: Option<i64>,
        message_ids: Vec<String>,
    ) -> Self {
        Self::LegacyUser {
            text,
            envelope_id,
            message_ids,
        }
    }

    pub fn observation(&self) -> Observation {
        match self {
            Self::User { text, .. } | Self::LegacyUser { text, .. } => {
                Observation::UserMessage { text: text.clone() }
            }
            Self::System { observation, .. } => observation.clone(),
        }
    }

    pub fn envelope_id(&self) -> Option<i64> {
        match self {
            Self::User { envelope_id, .. }
            | Self::LegacyUser { envelope_id, .. }
            | Self::System { envelope_id, .. } => *envelope_id,
        }
    }

    /// The message instances this entry is still holding. A set, because a fold merges two entries
    /// and the survivor must stay able to name BOTH; empty for a system entry.
    pub fn message_ids(&self) -> &[String] {
        match self {
            Self::User { message_ids, .. }
            | Self::LegacyUser { message_ids, .. }
            | Self::System { message_ids, .. } => message_ids,
        }
    }

    fn message_ids_mut(&mut self) -> &mut Vec<String> {
        match self {
            Self::User { message_ids, .. }
            | Self::LegacyUser { message_ids, .. }
            | Self::System { message_ids, .. } => message_ids,
        }
    }

    /// Give a user-authored entry a transfer identity if it has none, in the transaction that moves
    /// it. Minting here rather than at load keeps an id stable across reads.
    pub fn ensure_message_id(&mut self) -> &[String] {
        if self.is_user_authored() && self.message_ids().is_empty() {
            self.message_ids_mut().push(new_id());
        }
        self.message_ids()
    }

    /// Drop the message instances a give-back has already moved back, keeping the ENTRY (a fold
    /// can hold a returned id next to a never-harvested one). Returns whether the entry became
    /// id-less AS A RESULT of this call — an entry that never had ids must not be dropped.
    pub fn remove_message_ids(&mut self, returned: &HashSet<String>) -> bool {
        let had_ids = !self.message_ids().is_empty();
        self.message_ids_mut().retain(|id| !returned.contains(id));
        had_ids && self.message_ids().is_empty()
    }

    fn envelope_id_mut(&mut self) -> &mut Option<i64> {
        match self {
            Self::User { envelope_id, .. }
            | Self::LegacyUser { envelope_id, .. }
            | Self::System { envelope_id, .. } => envelope_id,
        }
    }

    pub fn id(&self) -> Option<&QueueEntryId> {
        match self {
            Self::User { id, .. } => Some(id),
            Self::LegacyUser { .. } | Self::System { .. } => None,
        }
    }

    /// `Some` exactly for [`QueueEntry::User`]; the read endpoint's filter.
    pub fn user_view(&self) -> Option<UserEntryView<'_>> {
        match self {
            Self::User {
                id,
                text,
                rev,
                queued_at_ms,
                attachments,
                ..
            } => Some(UserEntryView {
                id,
                text,
                rev: *rev,
                queued_at_ms: *queued_at_ms,
                attachments,
            }),
            Self::LegacyUser { .. } | Self::System { .. } => None,
        }
    }

    /// The bound attachments this entry carries; only [`QueueEntry::User`] can hold any.
    pub fn attachments(&self) -> &[BoundAttachment] {
        match self {
            Self::User { attachments, .. } => attachments,
            Self::LegacyUser { .. } | Self::System { .. } => &[],
        }
    }

    /// True for any entry authored by the user, addressable or not.
    pub fn is_user_authored(&self) -> bool {
        matches!(self, Self::User { .. } | Self::LegacyUser { .. })
    }

    /// Delegates to [`Observation::is_hard_fire`] for every variant, so the user arms cannot
    /// silently disagree with the observation table.
    pub fn is_hard_fire(&self) -> bool {
        match self {
            Self::User { .. } | Self::LegacyUser { .. } => Observation::UserMessage {
                text: String::new(),
            }
            .is_hard_fire(),
            Self::System { observation, .. } => observation.is_hard_fire(),
        }
    }

    pub fn report_sha256(&self) -> Option<&str> {
        match self {
            Self::User { .. } | Self::LegacyUser { .. } => None,
            Self::System { observation, .. } => observation.report_sha256(),
        }
    }

    /// Non-empty `WorkerHookStop` idempotency key, for the dedupe LRU.
    pub fn hook_idempotency_key(&self) -> Option<&str> {
        match self {
            Self::System {
                observation:
                    Observation::WorkerHookStop {
                        idempotency_key, ..
                    },
                ..
            } if !idempotency_key.is_empty() => Some(idempotency_key),
            _ => None,
        }
    }
}

/// One addressable change a human asked for. `if_entry_rev` is required on every arm: an
/// optional token is an unconditional write for any client that omits it.
#[derive(Debug, Clone, PartialEq)]
pub enum QueueMutation {
    Edit {
        entry_id: QueueEntryId,
        text: String,
        if_entry_rev: u32,
    },
    Delete {
        entry_id: QueueEntryId,
        if_entry_rev: u32,
    },
    /// Take the entry out so the run loop can hand it to the running turn (`turn/steer`); a codex
    /// refusal puts the entry back through `rebuffer_head`.
    Steer {
        entry_id: QueueEntryId,
        if_entry_rev: u32,
    },
}

impl QueueMutation {
    pub fn entry_id(&self) -> &QueueEntryId {
        match self {
            Self::Edit { entry_id, .. }
            | Self::Delete { entry_id, .. }
            | Self::Steer { entry_id, .. } => entry_id,
        }
    }

    fn if_entry_rev(&self) -> u32 {
        match self {
            Self::Edit { if_entry_rev, .. }
            | Self::Delete { if_entry_rev, .. }
            | Self::Steer { if_entry_rev, .. } => *if_entry_rev,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MutationApplied {
    pub entry_id: QueueEntryId,
    pub change: HarnessQueueChange,
    /// The entry's `rev` after the change; `Deleted` and `Steered` report the rev at removal.
    pub rev: u32,
    /// The text after the change, for `Edit` only.
    pub text: Option<String>,
    /// The entry that left the queue, for `Steer` only: the caller must deliver it, or put it
    /// back if codex will not take it. A `Delete` drops its entry here.
    pub removed: Option<QueueEntry>,
    pub queue_now_empty: bool,
    /// Whether any entry still in the queue is hard-fire, recomputed from the
    /// entries that remain. The caller re-arms the debounce with it.
    pub remaining_hard_fire: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MutationRefused {
    /// No entry in the queue carries this id. A drain is not final: `rebuffer_head` can put the
    /// batch back, so the entry may reappear.
    NotFound,
    /// The entry is there, but its text has moved on since the client read it.
    Stale {
        entry_id: QueueEntryId,
        text: String,
        rev: u32,
    },
    /// Two entries carry the same id. Unreachable today (uuid v4 minting, and the read boundary
    /// demotes duplicates); refused rather than resolved so a violation cannot write to the wrong message.
    AmbiguousId {
        entry_id: QueueEntryId,
        count: usize,
    },
}

/// The domain answer to a mutation, distinct from the transport `Result` around it ("the
/// request never reached the queue").
pub type MutationResult = std::result::Result<MutationApplied, MutationRefused>;

/// The locate-and-compare half of [`apply_mutation`], with no write; a steer calls it on its
/// own so "is the entry there" is answered ahead of "is a turn running".
pub fn locate_entry(
    queue: &VecDeque<QueueEntry>,
    entry_id: &QueueEntryId,
    if_entry_rev: u32,
) -> std::result::Result<usize, MutationRefused> {
    let matches = queue
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.id() == Some(entry_id))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let index = match matches.as_slice() {
        [] => return Err(MutationRefused::NotFound),
        [only] => *only,
        many => {
            return Err(MutationRefused::AmbiguousId {
                entry_id: entry_id.clone(),
                count: many.len(),
            });
        }
    };

    let view = queue[index]
        .user_view()
        .expect("an entry matched by id is a User entry, the only variant that has one");
    if view.rev != if_entry_rev {
        return Err(MutationRefused::Stale {
            entry_id: entry_id.clone(),
            text: view.text.to_string(),
            rev: view.rev,
        });
    }
    Ok(index)
}

/// Apply one human mutation in place. The caller holds the queue lock; this does no IO and
/// takes no locks, so the whole compare-and-swap happens inside one critical section.
pub fn apply_mutation(
    queue: &mut VecDeque<QueueEntry>,
    mutation: &QueueMutation,
) -> MutationResult {
    let entry_id = mutation.entry_id();
    let index = locate_entry(queue, entry_id, mutation.if_entry_rev())?;

    let (change, rev, text, removed) = match mutation {
        QueueMutation::Edit { text: new_text, .. } => {
            let QueueEntry::User { text, rev, .. } = &mut queue[index] else {
                unreachable!("an entry matched by id is a User entry")
            };
            new_text.clone_into(text);
            // The entry keeps its `message_ids`: an edit changes what the instance SAYS, not which
            // instance it is. Same rule as a fold: the body changed, so other clients' in-flight writes
            // against the old rev get a 409.
            *rev = rev.saturating_add(1);
            (
                HarnessQueueChange::Edited,
                *rev,
                Some(new_text.clone()),
                None,
            )
        }
        QueueMutation::Delete { .. } => {
            let removed = queue.remove(index).expect("index came from this queue");
            let rev = removed
                .user_view()
                .expect("an entry matched by id is a User entry")
                .rev;
            (HarnessQueueChange::Deleted, rev, None, None)
        }
        QueueMutation::Steer { .. } => {
            let removed = queue.remove(index).expect("index came from this queue");
            let rev = removed
                .user_view()
                .expect("an entry matched by id is a User entry")
                .rev;
            (HarnessQueueChange::Steered, rev, None, Some(removed))
        }
    };

    Ok(MutationApplied {
        entry_id: entry_id.clone(),
        change,
        rev,
        text,
        removed,
        queue_now_empty: queue.is_empty(),
        // Recomputed over what is LEFT, not patched.
        remaining_hard_fire: queue.iter().any(QueueEntry::is_hard_fire),
    })
}

/// What [`try_fold_tail`] did with an incoming entry.
#[derive(Debug, PartialEq)]
pub enum FoldOutcome {
    /// Nothing folded; the caller must find the entry a slot of its own.
    NotFolded,
    /// Merged into the queue tail. `entry_id` is the SURVIVING entry's id (the incoming one no
    /// longer exists); `None` when the survivor is a `LegacyUser`.
    Folded { entry_id: Option<QueueEntryId> },
}

/// A `ReportEdited` system entry: the one shape that folds into an adjacent same-track
/// predecessor on EVERY enqueue, not only under backpressure.
pub(crate) fn is_report_edit(entry: &QueueEntry) -> bool {
    matches!(
        entry,
        QueueEntry::System {
            observation: Observation::ReportEdited { .. },
            ..
        }
    )
}

/// The early fold: when `incoming` and the tail are both report edits, fold now rather than
/// only at the cap. Shared by the live enqueue and boot replay. The user-text cap is `0`
/// because it bounds the `User` arms only.
pub(crate) fn try_fold_report_edit_tail(
    queue: &mut VecDeque<QueueEntry>,
    incoming: &QueueEntry,
) -> FoldOutcome {
    if is_report_edit(incoming) && queue.back().is_some_and(is_report_edit) {
        try_fold_tail(queue, incoming, 0)
    } else {
        FoldOutcome::NotFolded
    }
}

/// Merge an incoming entry into the queue tail under backpressure. Text-bearing folds bump the
/// survivor's `rev`; `queued_at_ms` is deliberately NOT advanced, or a stream of folds would
/// keep an entry looking permanently fresh.
pub fn try_fold_tail(
    queue: &mut VecDeque<QueueEntry>,
    incoming: &QueueEntry,
    max_folded_user_chars: usize,
) -> FoldOutcome {
    let incoming_envelope_id = incoming.envelope_id();
    let Some(last) = queue.back_mut() else {
        return FoldOutcome::NotFolded;
    };
    let folded = match (last, incoming) {
        // Preserve both adjacent user intents rather than evicting the older send; capped so the
        // tail cannot grow unboundedly.
        (
            QueueEntry::User {
                text,
                rev,
                attachments,
                ..
            },
            QueueEntry::User {
                text: new_text,
                attachments: new_attachments,
                ..
            },
        ) => {
            // The attachment budget is checked BEFORE the text is touched because `fold_user_text`
            // mutates in place. The union is DEDUPLICATED: two adjacent messages can legitimately name
            // the same image, and `validate_attachment_list` refuses a repeat within one message.
            let merged = new_attachments
                .iter()
                .filter(|incoming| !attachments.iter().any(|held| held.id == incoming.id))
                .cloned()
                .collect::<Vec<_>>();
            if attachments.len() + merged.len() > MAX_ATTACHMENTS_PER_MESSAGE {
                false
            } else if fold_user_text(text, new_text, max_folded_user_chars) {
                attachments.extend(merged);
                *rev = rev.saturating_add(1);
                true
            } else {
                false
            }
        }
        (
            QueueEntry::LegacyUser { text, .. },
            QueueEntry::User {
                text: new_text,
                attachments: new_attachments,
                ..
            },
        ) => {
            // Text is appended but no id is assigned: a legacy entry never becomes addressable. A
            // `LegacyUser` has nowhere to put an attachment, so a message carrying one keeps its own slot.
            if new_attachments.is_empty() {
                fold_user_text(text, new_text, max_folded_user_chars)
            } else {
                false
            }
        }
        (
            QueueEntry::System {
                observation: Observation::TrackGoal { text },
                ..
            },
            QueueEntry::System {
                observation: Observation::TrackGoal { text: new_text },
                ..
            },
        ) => {
            *text = new_text.clone();
            true
        }
        // Adjacent report edits of one track fold, but only when the incoming edit CONTINUES the
        // held one (its `body_before` is the survivor's body); folding across an intervening write
        // would render that write's lines as the user's.
        (
            QueueEntry::System {
                observation:
                    Observation::ReportEdited {
                        track_id,
                        body_sha256,
                        body,
                        author,
                        body_before: _,
                        doc_rev_after,
                        blocks_after,
                    },
                ..
            },
            QueueEntry::System {
                observation:
                    Observation::ReportEdited {
                        track_id: new_track_id,
                        body_sha256: new_body_sha256,
                        body: new_body,
                        author: new_author,
                        body_before: new_body_before,
                        doc_rev_after: new_doc_rev_after,
                        blocks_after: new_blocks_after,
                    },
                ..
            },
        ) if track_id == new_track_id && new_body_before.as_deref() == Some(body.as_str()) => {
            *body_sha256 = new_body_sha256.clone();
            *body = new_body.clone();
            *author = *new_author;
            *doc_rev_after = *new_doc_rev_after;
            *blocks_after = new_blocks_after.clone();
            // But the OLDEST `body_before`: the diff must run from the version the planner last knew.
            // `None` is kept too — adopting the incoming one would start the diff at that entry's AFTER-body.
            true
        }
        _ => false,
    };
    if !folded {
        return FoldOutcome::NotFolded;
    }
    let survivor = queue
        .back_mut()
        .expect("fold matched a tail entry, so the queue is non-empty");
    *survivor.envelope_id_mut() = incoming_envelope_id;
    // The survivor carries BOTH sets of message ids (which instances am I still holding) while
    // the envelope id ADVANCES (which push am I acknowledging).
    let incoming_message_ids = incoming.message_ids().to_vec();
    survivor.message_ids_mut().extend(incoming_message_ids);
    FoldOutcome::Folded {
        entry_id: survivor.id().cloned(),
    }
}

/// The transcript view of one issued batch, attachments included; `Observation` has no
/// attachment field, so the batch path builds segments from entries.
pub fn input_segments_for_entries(
    card_id: &CardId,
    entries: &[QueueEntry],
) -> Vec<HarnessInputSegment> {
    entries
        .iter()
        .map(|entry| {
            let mut segments = Observation::input_segments_for(&[entry.observation()]);
            let mut segment = segments
                .pop()
                .expect("input_segments_for maps one observation to exactly one segment");
            segment.attachments = entry
                .attachments()
                .iter()
                .map(|attachment| attachment.wire(card_id))
                .collect();
            segment
        })
        .collect()
}

fn fold_user_text(text: &mut String, new_text: &str, max_folded_user_chars: usize) -> bool {
    let current_chars = text.chars().count();
    let new_chars = new_text.chars().count();
    if current_chars.saturating_add(new_chars).saturating_add(2) > max_folded_user_chars {
        return false;
    }
    text.push_str("\n\n");
    text.push_str(new_text);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> QueueEntry {
        QueueEntry::user_message(text.to_string(), None, Vec::new())
    }

    fn attachment(seed: char) -> BoundAttachment {
        let id = format!("0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6{seed}.png");
        BoundAttachment {
            id: calm_types::planner_attachment::AttachmentId::parse(&id).expect("valid id"),
            size: 11,
            path: format!("/w/.neige/attachments/card/bound/{id}"),
        }
    }

    fn user_with(text: &str, attachments: Vec<BoundAttachment>) -> QueueEntry {
        QueueEntry::user_message(text.to_string(), None, attachments)
    }

    /// The survivor has to hold both messages' images.
    #[test]
    fn a_fold_unions_both_messages_attachments() {
        let mut queue = VecDeque::from(vec![user_with("first", vec![attachment('0')])]);
        let outcome = try_fold_tail(
            &mut queue,
            &user_with("second", vec![attachment('1')]),
            10_000,
        );
        assert!(matches!(outcome, FoldOutcome::Folded { .. }));
        assert_eq!(queue.len(), 1);
        let ids = queue[0]
            .attachments()
            .iter()
            .map(|a| a.id.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 2, "both sets survive: {ids:?}");
        assert!(ids[0].ends_with("5e60.png") && ids[1].ends_with("5e61.png"));
    }

    #[test]
    fn a_fold_does_not_hold_the_same_attachment_twice() {
        let shared = attachment('0');
        let mut queue = VecDeque::from(vec![user_with("first", vec![shared.clone()])]);
        let outcome = try_fold_tail(
            &mut queue,
            &user_with("second", vec![shared.clone(), attachment('1')]),
            10_000,
        );
        assert!(matches!(outcome, FoldOutcome::Folded { .. }));
        let ids = queue[0]
            .attachments()
            .iter()
            .map(|a| a.id.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            ids.len(),
            2,
            "the repeat is dropped, the new one is kept: {ids:?}"
        );
        assert!(ids[0].ends_with("5e60.png"), "{ids:?}");
        assert!(ids[1].ends_with("5e61.png"), "{ids:?}");
    }

    #[test]
    fn the_fold_cap_counts_what_the_survivor_would_actually_hold() {
        let held = ['0', '1', '2', '3', '4', '5', '6', '7']
            .into_iter()
            .map(attachment)
            .collect::<Vec<_>>();
        let mut queue = VecDeque::from(vec![user_with("first", held.clone())]);
        assert!(
            matches!(
                try_fold_tail(&mut queue, &user_with("second", held), 10_000),
                FoldOutcome::Folded { .. },
            ),
            "8 + 8 identical ids is 8, not 16"
        );
        assert_eq!(queue[0].attachments().len(), 8);
    }

    /// Folding and then truncating the list would silently drop images.
    #[test]
    fn a_fold_that_would_exceed_the_cap_is_declined_rather_than_truncated() {
        // DISJOINT sets: the cap counts the deduplicated union. 5 + 4 distinct = 9 > 8.
        let mut queue = VecDeque::from(vec![user_with(
            "first",
            ['0', '1', '2', '3', '4']
                .into_iter()
                .map(attachment)
                .collect(),
        )]);
        let incoming = user_with(
            "second",
            ['5', '6', '7', '8'].into_iter().map(attachment).collect(),
        );
        assert_eq!(
            try_fold_tail(&mut queue, &incoming, 10_000),
            FoldOutcome::NotFolded,
            "5 + 4 > 8, so the tail is left alone"
        );
        assert_eq!(queue[0].attachments().len(), 5);
        assert_eq!(queue[0].user_view().unwrap().text, "first");
        assert_eq!(
            queue[0].user_view().unwrap().rev,
            0,
            "a declined fold bumps nothing"
        );
    }

    #[test]
    fn folding_an_attachment_bearing_message_onto_a_legacy_tail_is_declined() {
        let mut queue = VecDeque::from(vec![QueueEntry::legacy_user(
            "older".into(),
            None,
            Vec::new(),
        )]);
        assert_eq!(
            try_fold_tail(
                &mut queue,
                &user_with("with image", vec![attachment('0')]),
                10_000
            ),
            FoldOutcome::NotFolded,
        );
        assert_eq!(queue[0].user_view().map(|view| view.text), None);
        // Without an attachment the same fold still happens, so the guard is
        // scoped to the case that would lose something.
        assert!(matches!(
            try_fold_tail(&mut queue, &user("plain"), 10_000),
            FoldOutcome::Folded { entry_id: None },
        ));
    }

    #[test]
    fn segments_carry_each_entrys_own_attachments() {
        let entries = vec![
            user_with("has one", vec![attachment('0')]),
            user("has none"),
        ];
        let segments = input_segments_for_entries(&CardId::from("card-seg"), &entries);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].attachments.len(), 1);
        assert_eq!(segments[0].attachments[0].content_type, "image/png");
        assert!(segments[1].attachments.is_empty());
        // Delegated, not restated: the rendered text is whatever
        // `Observation::input_segments_for` produces for the same observation.
        let expected = Observation::input_segments_for(&[entries[0].observation()]);
        assert_eq!(segments[0].text, expected[0].text);
        assert_eq!(segments[0].presentation, expected[0].presentation);
    }

    #[test]
    fn the_wire_shape_of_an_attachment_carries_no_host_path() {
        let json =
            serde_json::to_string(&attachment('0').wire(&CardId::from("card-wire"))).unwrap();
        assert!(!json.contains("/w/"), "{json}");
        assert!(!json.contains("path"), "{json}");
        assert!(json.contains("\"contentType\":\"image/png\""), "{json}");
    }

    #[test]
    fn pending_entry_system_refuses_user_message() {
        let refused = QueueEntry::system(
            Observation::UserMessage {
                text: "hello".into(),
            },
            None,
        );
        assert!(
            refused.is_err(),
            "a UserMessage must not be wrappable as a System entry"
        );

        let accepted = QueueEntry::system(
            Observation::TrackGoal {
                text: "ship it".into(),
            },
            Some(7),
        )
        .expect("a non-user observation is accepted");
        assert_eq!(accepted.id(), None, "system entries never carry an id");
        assert_eq!(accepted.envelope_id(), Some(7));
    }

    #[test]
    fn user_message_mints_a_fresh_id_each_time() {
        let a = user("one");
        let b = user("two");
        assert_ne!(a.id(), b.id());
        assert!(a.user_view().is_some());
        assert_eq!(a.user_view().expect("user view").rev, 0);
    }

    #[test]
    fn legacy_user_entries_are_not_addressable_and_have_no_id_slot() {
        let legacy = QueueEntry::legacy_user("older send".into(), Some(3), Vec::new());
        assert_eq!(legacy.id(), None);
        assert!(legacy.user_view().is_none(), "must not reach the read page");
        assert!(
            legacy.is_user_authored(),
            "but it still counts as the user's"
        );
        assert!(legacy.is_hard_fire());
        assert_eq!(
            legacy.observation(),
            Observation::UserMessage {
                text: "older send".into()
            }
        );
    }

    #[test]
    fn folding_two_user_entries_keeps_the_survivor_id_and_bumps_rev() {
        let mut queue = VecDeque::from(vec![QueueEntry::user_message(
            "first".into(),
            Some(1),
            Vec::new(),
        )]);
        let survivor_id = queue[0].id().cloned().expect("user entry has an id");
        let incoming = QueueEntry::user_message("second".into(), Some(2), Vec::new());
        let incoming_id = incoming.id().cloned().expect("user entry has an id");

        let outcome = try_fold_tail(&mut queue, &incoming, 4 * 32_768);

        assert_eq!(
            outcome,
            FoldOutcome::Folded {
                entry_id: Some(survivor_id.clone())
            },
            "the ack names the surviving entry, never the discarded new id"
        );
        assert_ne!(survivor_id, incoming_id);
        assert_eq!(queue.len(), 1);
        let view = queue[0].user_view().expect("survivor is still addressable");
        assert_eq!(view.text, "first\n\nsecond");
        assert_eq!(view.rev, 1, "a rewritten body invalidates a stale editor");
        assert_eq!(
            queue[0].envelope_id(),
            Some(2),
            "the folded entry advances to the newest send's envelope id"
        );
    }

    #[test]
    fn folding_onto_a_legacy_tail_appends_text_but_acks_none() {
        let mut queue = VecDeque::from(vec![QueueEntry::legacy_user(
            "older".into(),
            None,
            Vec::new(),
        )]);
        let incoming = user("newer");

        let outcome = try_fold_tail(&mut queue, &incoming, 4 * 32_768);

        assert_eq!(outcome, FoldOutcome::Folded { entry_id: None });
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].id(), None, "a legacy entry never gains an id");
        assert!(
            queue[0].user_view().is_none(),
            "and it stays off the addressable page after the fold"
        );
        assert_eq!(
            queue[0].observation(),
            Observation::UserMessage {
                text: "older\n\nnewer".into()
            }
        );
    }

    #[test]
    fn folding_unions_the_message_ids_of_both_instances() {
        let mut queue = VecDeque::from(vec![user("first")]);
        let first_instance = queue[0].message_ids().to_vec();
        assert_eq!(
            first_instance.len(),
            1,
            "premise: the tail has one instance"
        );
        let incoming = user("second");
        let second_instance = incoming.message_ids().to_vec();
        assert_eq!(
            second_instance.len(),
            1,
            "premise: so does the incoming one"
        );

        try_fold_tail(&mut queue, &incoming, 4 * 32_768);

        assert_eq!(queue.len(), 1);
        let mut expected = first_instance;
        expected.extend(second_instance);
        assert_eq!(
            queue[0].message_ids(),
            expected.as_slice(),
            "the survivor must still be able to name both moved instances"
        );
    }

    #[test]
    fn a_transfer_boundary_mints_a_message_id_for_a_legacy_entry() {
        let mut legacy = QueueEntry::legacy_user("pre-#1449".into(), None, Vec::new());
        assert!(legacy.message_ids().is_empty(), "premise: no identity yet");

        let minted = legacy.ensure_message_id().to_vec();

        assert_eq!(minted.len(), 1, "a moved instance is always identifiable");
        assert_eq!(legacy.id(), None, "and it is still not addressable (GAP-B)");
        assert!(legacy.user_view().is_none());

        // Idempotent: a second boundary does not mint a second identity, or
        // the give-back would be matching on an id the source never recorded.
        let again = legacy.ensure_message_id().to_vec();
        assert_eq!(again, minted);
    }

    #[test]
    fn a_transfer_boundary_mints_nothing_for_a_system_entry() {
        let mut system = QueueEntry::system(
            Observation::TrackGoal {
                text: "goal".into(),
            },
            None,
        )
        .expect("system entry");
        assert!(system.ensure_message_id().is_empty());
    }

    #[test]
    fn folding_refuses_once_the_char_cap_would_be_exceeded() {
        let mut queue = VecDeque::from(vec![user("aaaa")]);
        let incoming = user("bbbb");
        assert_eq!(
            try_fold_tail(&mut queue, &incoming, 8),
            FoldOutcome::NotFolded,
            "4 + 4 + the two-char separator exceeds a cap of 8"
        );
        assert_eq!(
            queue[0].user_view().expect("unchanged").rev,
            0,
            "a refused fold must not bump rev"
        );
    }

    #[test]
    fn a_user_entry_never_folds_into_a_system_tail() {
        let mut queue = VecDeque::from(vec![
            QueueEntry::system(
                Observation::TrackGoal {
                    text: "goal".into(),
                },
                None,
            )
            .expect("system entry"),
        ]);
        assert_eq!(
            try_fold_tail(&mut queue, &user("hi"), 4 * 32_768),
            FoldOutcome::NotFolded
        );
    }

    // `Steer` is a delete that hands the entry back; each refusal is pinned on the steer arm.

    #[test]
    fn a_steer_takes_the_entry_out_and_hands_it_back() {
        let mut queue = VecDeque::from(vec![user("first"), user("second"), user("third")]);
        let target = queue[1].clone();
        let entry_id = target.id().cloned().expect("user entry has an id");

        let applied = apply_mutation(
            &mut queue,
            &QueueMutation::Steer {
                entry_id: entry_id.clone(),
                if_entry_rev: 0,
            },
        )
        .expect("applies");

        assert_eq!(applied.change, HarnessQueueChange::Steered);
        assert_eq!(applied.entry_id, entry_id);
        assert_eq!(applied.rev, 0);
        assert_eq!(applied.text, None);
        assert_eq!(
            applied.removed.as_ref(),
            Some(&target),
            "the caller gets the same instance, id and rev included"
        );
        assert!(!applied.queue_now_empty);
        assert!(
            applied.remaining_hard_fire,
            "two user entries are still waiting"
        );
        assert_eq!(queue.len(), 2);
        assert!(
            queue.iter().all(|entry| entry.id() != Some(&entry_id)),
            "and it is no longer in the queue"
        );
    }

    #[test]
    fn a_restore_bumps_the_rev_of_a_user_entry_and_nothing_else() {
        let mut entry = user("came back");
        let before = entry.clone();
        entry.bump_rev_for_restore();
        let (Some(was), Some(now)) = (before.user_view(), entry.user_view()) else {
            panic!("a user entry has a user view");
        };
        assert_eq!(now.rev, was.rev + 1, "the CAS token moved");
        assert_eq!(now.id, was.id, "the same instance");
        assert_eq!(now.text, was.text, "the same text");
        assert_eq!(entry.message_ids(), before.message_ids());

        let mut legacy = QueueEntry::legacy_user("older".into(), None, Vec::new());
        let legacy_before = legacy.clone();
        legacy.bump_rev_for_restore();
        assert_eq!(legacy, legacy_before, "no rev to move");
    }

    #[test]
    fn a_steer_against_a_stale_rev_is_refused_and_changes_nothing() {
        let mut queue = VecDeque::from(vec![user("keep")]);
        let entry_id = queue[0].id().cloned().expect("user entry has an id");
        if let QueueEntry::User { rev, .. } = &mut queue[0] {
            *rev = 2;
        }

        let refused = apply_mutation(
            &mut queue,
            &QueueMutation::Steer {
                entry_id: entry_id.clone(),
                if_entry_rev: 1,
            },
        )
        .expect_err("stale rev");

        assert_eq!(
            refused,
            MutationRefused::Stale {
                entry_id,
                text: "keep".into(),
                rev: 2,
            }
        );
        assert_eq!(queue.len(), 1, "a refused steer removes nothing");
    }

    #[test]
    fn a_steer_of_an_unknown_id_is_not_found_and_a_delete_hands_nothing_back() {
        let mut queue = VecDeque::from(vec![user("only")]);
        let entry_id = queue[0].id().cloned().expect("user entry has an id");
        assert_eq!(
            apply_mutation(
                &mut queue,
                &QueueMutation::Steer {
                    entry_id: QueueEntryId::from_wire("no-such-entry".into()),
                    if_entry_rev: 0,
                },
            ),
            Err(MutationRefused::NotFound)
        );
        assert_eq!(queue.len(), 1);

        // The paired negative for `removed`: a delete must never hand the
        // entry to anything that could deliver it.
        let applied = apply_mutation(
            &mut queue,
            &QueueMutation::Delete {
                entry_id,
                if_entry_rev: 0,
            },
        )
        .expect("applies");
        assert_eq!(applied.change, HarnessQueueChange::Deleted);
        assert_eq!(applied.removed, None);
        assert!(applied.queue_now_empty);
    }

    #[test]
    fn locate_entry_answers_the_same_refusals_without_writing() {
        let mut queue = VecDeque::from(vec![user("a"), user("b")]);
        let entry_id = queue[1].id().cloned().expect("user entry has an id");
        assert_eq!(locate_entry(&queue, &entry_id, 0), Ok(1));
        assert_eq!(
            locate_entry(&queue, &entry_id, 3),
            Err(MutationRefused::Stale {
                entry_id: entry_id.clone(),
                text: "b".into(),
                rev: 0,
            })
        );
        assert_eq!(
            locate_entry(&queue, &QueueEntryId::from_wire("nope".into()), 0),
            Err(MutationRefused::NotFound)
        );
        // Two entries with one id is refused rather than resolved, exactly as
        // `apply_mutation` refuses it.
        let twin = queue[1].clone();
        queue.push_back(twin);
        assert_eq!(
            locate_entry(&queue, &entry_id, 0),
            Err(MutationRefused::AmbiguousId { entry_id, count: 2 })
        );
        assert_eq!(queue.len(), 3, "a locate never writes");
    }
}

#[cfg(test)]
mod report_edit_fold_tests;
