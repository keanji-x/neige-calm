//! #1505 PR1 — identity for entries sitting in the harness pending queue.
//!
//! Before this module the queue was several parallel arrays kept in step by
//! hand (`pending_queue` / `pending_envelope_ids` / `pending_message_ids`,
//! plus the alignment pass that papered over any head-side drift). Every
//! mutation site had to touch each array in the same way, and the failure mode
//! was silent: a head-side drain on one array only was re-lengthened by the
//! alignment pass, so ids shifted by one and a later delete-by-id would hit
//! somebody else's message.
//!
//! [`QueueEntry`] fuses them into one value, so there is a single write point
//! and a single ordering.
//!
//! # A constraint on PR2's mutation path
//!
//! Address-by-id is only unambiguous while one id names one entry. Minting
//! cannot break that (uuid v4), and
//! `HarnessSnapshot::deserialize_pending_entry_meta` demotes a duplicate that
//! serde smuggles in, so today the queue holds no two entries with the same
//! id. That is a property of the READ boundary, not a property this type
//! enforces. When PR2 adds `apply_mutation`, its lookup must therefore choose
//! deliberately — first match, or refuse on more than one — and say which in
//! code. What it must not do is scan for "the" match and rely on there being
//! exactly one, because that reintroduces "delete hits somebody else's
//! message" by coincidence rather than by construction.

use std::collections::{HashSet, VecDeque};

use crate::error::{CalmError, Result};
use crate::event::HarnessQueueChange;
use crate::harness::observation::Observation;
use crate::model::{new_id, now_ms};

/// Stable identity for one addressable user entry in the pending queue.
///
/// Not required to sort: no reader in the design orders by id (the read
/// endpoint emits queue order, addressing is equality matching).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct QueueEntryId(String);

impl QueueEntryId {
    /// Mint a fresh id. Deliberately crate-visible: production code reaches
    /// this only through [`QueueEntry::user_message`], which is the one
    /// place an id is created.
    pub(crate) fn mint() -> Self {
        Self(new_id())
    }

    /// Adopt an id that arrived from a client, verbatim.
    ///
    /// No validation, and none is possible: the set of valid ids is exactly
    /// "the ids currently in this queue", which only the queue can answer. It
    /// answers by not matching, which is a 404 — so an id from a URL is a
    /// lookup key here and never a claim about anything.
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

/// One entry in the harness pending queue.
///
/// Three variants, and the third is the interesting one:
///
/// - [`QueueEntry::User`] — a user message minted at or after #1505 PR1. It
///   carries an id, so it can be shown, addressed and (from PR2) edited or
///   deleted.
/// - [`QueueEntry::System`] — everything the dispatcher enqueues. Never
///   addressable, never carries an id. [`QueueEntry::system`] refuses a
///   `UserMessage`, which is a guard on that constructor and nothing wider:
///   the variants are `pub`, so `QueueEntry::System { observation:
///   Observation::UserMessage { .. }, .. }` can be written literally, and it
///   would be a user message that `is_user_authored` and `user_view` both deny
///   exists. No such literal exists in this repo, and the state is bounded —
///   the first `set_pending_entries` writes its meta slot as `None`, so the
///   next read returns it as a `LegacyUser` and it rejoins the count.
/// - [`QueueEntry::LegacyUser`] — a `UserMessage` read back from a snapshot
///   whose parallel `pending_entry_meta` slot is `None`. It has **no
///   `QueueEntryId` field at all**, and that structural fact — not a runtime
///   `Option` check — is what makes GAP-B's universal sentence ("they never
///   change id, because they never had one") true. It does carry
///   `message_ids`, which is a different identity for a different reader: see
///   [`QueueEntry::message_ids`] and `HarnessSnapshot::pending_message_ids`.
///   Whether a legacy entry stays legacy across a transfer is
///   PATH-DEPENDENT, and the difference is worth naming rather than
///   generalising away:
///
///   * [`QueueEntry::ensure_message_id`] and the **reset / inherit** boundary
///     (`planner_harness_start_adapter`'s `prepare_tx`) carry the entry WHOLE.
///     It gains a message id and no `QueueEntryId`; `user_view` still denies
///     it; it stays off the addressable page. GAP-B holds verbatim.
///   * The **harvest** boundary does not. `stranded_user_messages` admits a
///     `LegacyUser` via `is_user_authored` — it must, or every pre-#1505
///     sentence would be stranded on a superseded row, which is the loss #1449
///     exists to stop — and the successor rebuilds the entry through
///     [`QueueEntry::user_message_moved`], as a `User` with a freshly minted
///     `QueueEntryId`. That sentence IS listed in `GET /planner/run`'s
///     `pending` afterwards.
///
///   The second is deliberate, not a leak. GAP-B's reason is that an id
///   invented at READ time would differ on every read and could not be
///   addressed; an id minted once, inside the transaction that moves the
///   entry, and persisted with it has neither problem. So the harvest makes a
///   pre-#1505 sentence editable where it previously was not, which is
///   strictly better for the person who typed it. What GAP-B still forbids —
///   and what nothing here does — is repairing a legacy entry IN PLACE, on a
///   row it is not leaving.
///
///   **A `User` entry keeps its id across the same boundary** (#1505 PR4
///   review). The journal carries `entry_id`, so only an entry that arrives
///   without one is minted a new one. The earlier unconditional re-mint was a
///   bug rather than a policy: a client holding the pre-harvest id saw the
///   sentence drawn twice and got a 404 — reported in the UI as "already left
///   the queue" — for a message that was still queued.
///
///   Two things this variant is NOT:
///
///   1. It is not "only produced by snapshots written before PR1". A
///      `LegacyUser` that is re-persisted after PR1 is written back the same
///      way — text in `pending_queue`, `None` in the meta slot — so it reads
///      back as `LegacyUser` again, indefinitely, until it drains.
///   2. Its carrier is not "the only construction entry point". `calm_server`
///      exports `pub mod harness`, and an enum variant is as visible as its
///      enum, so `QueueEntry::LegacyUser { .. }` can be written literally by
///      any module. [`QueueEntry::legacy_user`] is merely the least effortful
///      path; the invariant rests on the type, which has nowhere to put an id.
#[derive(Clone, Debug, PartialEq)]
pub enum QueueEntry {
    User {
        id: QueueEntryId,
        text: String,
        /// CAS token. Incremented every time the text is rewritten, folding
        /// included, so a stale editor is told to re-read.
        rev: u32,
        /// Wall-clock ms at which this entry entered the queue.
        queued_at_ms: i64,
        envelope_id: Option<i64>,
        /// #1449 transfer identity. See [`QueueEntry::message_ids`].
        message_ids: Vec<String>,
    },
    LegacyUser {
        text: String,
        envelope_id: Option<i64>,
        /// #1449 transfer identity. Empty until this entry crosses a transfer
        /// boundary, which mints one — a legacy entry gains a message id even
        /// though it never gains a [`QueueEntryId`].
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
///
/// The read endpoint filters with this rather than by asking whether a meta
/// slot happens to be `None`: the former is a structural match on the variant,
/// the latter would silently start admitting legacy entries the moment anybody
/// wrote a meta slot for one.
pub struct UserEntryView<'a> {
    pub id: &'a QueueEntryId,
    pub text: &'a str,
    pub rev: u32,
    pub queued_at_ms: i64,
}

impl QueueEntry {
    /// The one place a [`QueueEntryId`] is minted.
    pub fn user_message(text: String, envelope_id: Option<i64>) -> Self {
        Self::User {
            id: QueueEntryId::mint(),
            text,
            rev: 0,
            queued_at_ms: now_ms(),
            envelope_id,
            // #1449 — a `UserMessage` entering the queue gets its transfer
            // identity here, in the same constructor that mints its
            // `QueueEntryId`. The two are minted together and are still two
            // ids: see `HarnessSnapshot::pending_message_ids` for why.
            message_ids: vec![new_id()],
        }
    }

    /// #1449 — a user message arriving in a successor's queue by MOVE.
    ///
    /// It keeps the transfer identity it was moved with: minting a fresh one
    /// would leave the give-back unable to recognise the instance it recorded,
    /// which is the whole point of the ids. (An empty set would mean the mover
    /// failed to mint at the boundary, so the fresh identity minted by
    /// [`Self::user_message`] is kept in that case rather than leaving the
    /// entry unidentifiable.)
    ///
    /// The [`QueueEntryId`] comes from `entry_id` when the mover has one to
    /// give (#1505 PR4 review): the harvest journal carries it, so a sentence
    /// that was addressable before the move is addressable under the SAME id
    /// after it, and a client still holding that id hits its own entry. `None`
    /// means the mover had none — a pre-#1505 sentence — and the fresh id
    /// minted by [`Self::user_message`] stands, which is where such a sentence
    /// becomes addressable for the first time.
    pub fn user_message_moved(
        text: String,
        message_ids: Vec<String>,
        entry_id: Option<QueueEntryId>,
    ) -> Self {
        let mut entry = Self::user_message(text, None);
        if !message_ids.is_empty() {
            *entry.message_ids_mut() = message_ids;
        }
        // #1505 PR4 review — adopt the id the sentence already had, and mint
        // only for one that never had any. See the `entry_id` field on
        // `HarvestedMessage` for why a move must not rename.
        if let Some(id) = entry_id
            && let Self::User { id: slot, .. } = &mut entry
        {
            *slot = id;
        }
        entry
    }

    /// Wrap a dispatcher observation.
    ///
    /// A `UserMessage` is refused. This is a **runtime** fail-closed guard that
    /// is live in release builds — not a compile-time signal. `Result` says
    /// only "this can fail"; every caller already handles a `Result`, so adding
    /// this arm reddens no build. The first sign that a dispatcher learned to
    /// mint `UserMessage` would be an `Err` on the boot-replay path, which is
    /// why `pending_entry_system_refuses_user_message` pins it explicitly.
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

    /// Fixtures-only bulk wrapper for seeding a snapshot from bare
    /// observations.
    ///
    /// It dispatches on the variant exactly as the ingress does — a
    /// `UserMessage` mints an id, everything else becomes a system entry — so a
    /// test seeds the same shapes production would. It is feature-gated rather
    /// than public because production has no path that starts from an
    /// undifferentiated observation: user input arrives at
    /// `observe_user_message_durable`, dispatcher output at `observe`.
    #[cfg(feature = "fixtures")]
    pub fn entries_from_observations_for_test(observations: Vec<Observation>) -> Vec<Self> {
        observations
            .into_iter()
            .map(|observation| match observation {
                Observation::UserMessage { text } => Self::user_message(text, None),
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

    /// #1449 — the message instances this entry is still holding.
    ///
    /// A **set**, not one id, because a fold merges two entries into one and
    /// the survivor must stay able to name BOTH instances: a give-back that
    /// could name only one would strand the other. Empty for a system entry,
    /// and for a user entry read back from a row written before #1449 — until
    /// [`QueueEntry::ensure_message_id`] mints one at a transfer boundary.
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

    /// #1449 — give a user-authored entry a transfer identity if it has none,
    /// in the transaction that moves it.
    ///
    /// Only user-authored entries: a system entry is never given back, so an
    /// id for it would be a claim with no reader. Minting HERE rather than at
    /// load is what keeps an id stable across reads — a read that minted would
    /// hand out a different id every time.
    ///
    /// Returns the entry's ids after the mint, which is what the mover records.
    pub fn ensure_message_id(&mut self) -> &[String] {
        if self.is_user_authored() && self.message_ids().is_empty() {
            self.message_ids_mut().push(new_id());
        }
        self.message_ids()
    }

    /// #1449 — drop the message instances a give-back has already moved back,
    /// keeping the ENTRY.
    ///
    /// Subtracting rather than dropping the entry, because a fold unions two
    /// instances into one entry: an entry can hold a returned id next to a
    /// newly enqueued one that was never harvested and sits on no source row,
    /// and dropping it on an intersection would delete that sentence outright.
    ///
    /// Returns whether the entry became id-less AS A RESULT of this call — it
    /// held ids and now holds none. That is not the same question as "does it
    /// hold no ids", and the difference is load-bearing: an entry that never
    /// had an identity was enqueued before #1449, was NOT returned (the
    /// give-back only returns ids the failing runtime still holds), and its
    /// source row has already been emptied, so dropping it would delete it
    /// from both sides.
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
                ..
            } => Some(UserEntryView {
                id,
                text,
                rev: *rev,
                queued_at_ms: *queued_at_ms,
            }),
            Self::LegacyUser { .. } | Self::System { .. } => None,
        }
    }

    /// True for any entry authored by the user, addressable or not. Used by
    /// the read endpoint to count what it could not show.
    pub fn is_user_authored(&self) -> bool {
        matches!(self, Self::User { .. } | Self::LegacyUser { .. })
    }

    /// Delegates to [`Observation::is_hard_fire`] for every variant, user
    /// entries included.
    ///
    /// The user arms could hardcode `true` and be right today, but that would
    /// be a restatement of somebody else's answer: moving `UserMessage` into
    /// the soft list would change the queue's behaviour and leave this
    /// function silently disagreeing. Asking costs an empty `String`, which
    /// does not allocate — the text is not needed to classify the variant.
    pub fn is_hard_fire(&self) -> bool {
        match self {
            Self::User { .. } | Self::LegacyUser { .. } => Observation::UserMessage {
                text: String::new(),
            }
            .is_hard_fire(),
            Self::System { observation, .. } => observation.is_hard_fire(),
        }
    }

    /// Same delegation as [`Self::is_hard_fire`]: a user entry's answer comes
    /// from `Observation`, not from a second opinion written here.
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

/// #1505 PR2 — one addressable change a human asked for.
///
/// Both arms carry `if_entry_rev`, and it is required rather than optional on
/// the delete too. "I am deleting the entry I read" and "I am editing the
/// entry I read" are the same precondition, and an optional token is an
/// unconditional write for any client that omits it.
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
}

impl QueueMutation {
    pub fn entry_id(&self) -> &QueueEntryId {
        match self {
            Self::Edit { entry_id, .. } | Self::Delete { entry_id, .. } => entry_id,
        }
    }

    fn if_entry_rev(&self) -> u32 {
        match self {
            Self::Edit { if_entry_rev, .. } | Self::Delete { if_entry_rev, .. } => *if_entry_rev,
        }
    }
}

/// A mutation that took effect.
#[derive(Debug, Clone, PartialEq)]
pub struct MutationApplied {
    pub entry_id: QueueEntryId,
    pub change: HarnessQueueChange,
    /// The entry's `rev` after the change. `Deleted` reports the rev the entry
    /// carried when it was removed, so a log line can be joined against the
    /// read the client acted on.
    pub rev: u32,
    /// The text after the change, for `Edit` only.
    pub text: Option<String>,
    /// True when this mutation left the queue empty.
    pub queue_now_empty: bool,
    /// Whether any entry still in the queue is hard-fire, recomputed from the
    /// entries that remain. The caller re-arms the debounce with it.
    pub remaining_hard_fire: bool,
}

/// Why a mutation did nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum MutationRefused {
    /// No entry in the queue carries this id. Already drained, dropped by a
    /// snapshot truncation, or gone with a restart — the three are not
    /// distinguishable here and deliberately are not reported as if they were.
    /// Note that a drain is not final: `rebuffer_head` can put the batch back,
    /// so the entry may reappear.
    NotFound,
    /// The entry is there, but its text has moved on since the client read it.
    Stale {
        entry_id: QueueEntryId,
        text: String,
        rev: u32,
    },
    /// Two entries in the queue carry the same id, so "the" entry the client
    /// named does not exist.
    ///
    /// Unreachable today, and the point is that it is refused rather than
    /// resolved: ids are minted as uuid v4, and
    /// `HarnessSnapshot::deserialize_pending_entry_meta` demotes a duplicate a
    /// hand-edited `handle_state_json` smuggled past it, so the read boundary
    /// is what makes ids unique — not this type. Picking the first match would
    /// turn a violation of somebody else's invariant into a write against
    /// whichever message happened to be earlier, which is precisely the
    /// "delete hits the wrong message" failure #1505 PR1 set out to make
    /// impossible.
    AmbiguousId {
        entry_id: QueueEntryId,
        count: usize,
    },
}

/// The domain answer to a mutation: it happened, or it was refused and why.
///
/// Distinct from the transport `Result` the harness returns around it. The
/// outer one means "the request never reached the queue" (the runtime is gone,
/// the channel is saturated); this one means the queue looked at the request
/// and answered.
pub type MutationResult = std::result::Result<MutationApplied, MutationRefused>;

/// Apply one human mutation to the pending queue, in place.
///
/// The queue lock is the caller's to hold; this function does no IO and takes
/// no locks, so the whole compare-and-swap — locate, check `rev`, write —
/// happens inside one critical section. That is what makes the delete-versus-
/// drain race have two outcomes instead of three: whichever of the two reaches
/// the run loop's single `select!` first sees the queue the other has not
/// touched yet.
pub fn apply_mutation(
    queue: &mut VecDeque<QueueEntry>,
    mutation: &QueueMutation,
) -> MutationResult {
    let entry_id = mutation.entry_id();
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

    let current_rev = queue[index]
        .user_view()
        .expect("an entry matched by id is a User entry, the only variant that has one")
        .rev;
    if current_rev != mutation.if_entry_rev() {
        let view = queue[index].user_view().expect("checked just above");
        return Err(MutationRefused::Stale {
            entry_id: entry_id.clone(),
            text: view.text.to_string(),
            rev: view.rev,
        });
    }

    let (change, rev, text) = match mutation {
        QueueMutation::Edit { text: new_text, .. } => {
            let QueueEntry::User { text, rev, .. } = &mut queue[index] else {
                unreachable!("an entry matched by id is a User entry")
            };
            new_text.clone_into(text);
            // The entry keeps its `message_ids`: an edit changes what the
            // instance SAYS, not which instance it is, and #1449's give-back
            // matches on those ids.
            //
            // KNOWN GAP (#1449 x #1505 PR2): the harvest journal records the
            // text as it was at the transfer, so if this entry was harvested
            // and the mint later fails, the give-back puts the PRE-edit text
            // back on the source row. Bounded — the entry is identified
            // correctly and nothing is lost or delivered twice, only the edit
            // is — and closing it means journalling by reference to a row that
            // the failing mint is in the middle of emptying. A DELETE has no
            // such gap: the ids leave with the entry, the give-back's "are
            // these ids still held" answers no, and the sentence is correctly
            // not restored.
            //
            // Same rule as a fold: the body a client was editing changed, so
            // any other client's in-flight write against the old rev is now
            // stale and gets a 409 instead of overwriting this one.
            *rev = rev.saturating_add(1);
            (HarnessQueueChange::Edited, *rev, Some(new_text.clone()))
        }
        QueueMutation::Delete { .. } => {
            let removed = queue.remove(index).expect("index came from this queue");
            let rev = removed
                .user_view()
                .expect("an entry matched by id is a User entry")
                .rev;
            (HarnessQueueChange::Deleted, rev, None)
        }
    };

    Ok(MutationApplied {
        entry_id: entry_id.clone(),
        change,
        rev,
        text,
        queue_now_empty: queue.is_empty(),
        // Recomputed over what is LEFT, not patched. An edit cannot change the
        // answer (the entry stays, and it was hard-fire before and after), but
        // computing it the same way in both arms keeps the caller from having
        // to know which arm can move it.
        remaining_hard_fire: queue.iter().any(QueueEntry::is_hard_fire),
    })
}

/// What [`try_fold_tail`] did with an incoming entry.
#[derive(Debug, PartialEq)]
pub enum FoldOutcome {
    /// Nothing folded; the caller must find the entry a slot of its own.
    NotFolded,
    /// The incoming entry was merged into the queue tail. `entry_id` is the
    /// **surviving** entry's id, which is what the client must be told about:
    /// the id minted for the incoming message no longer exists. It is `None`
    /// when the survivor is a [`QueueEntry::LegacyUser`], which never gains
    /// an id (GAP-B).
    Folded { entry_id: Option<QueueEntryId> },
}

/// #615 F3 — merge an incoming entry into the queue tail under backpressure.
///
/// Text-bearing folds bump the survivor's `rev` so a client that had already
/// read the old text gets a 409 out of a later CAS write: the body it was
/// editing genuinely changed.
///
/// `queued_at_ms` is deliberately NOT advanced. The survivor keeps the moment
/// it reached the queue, so a folded entry carries text newer than its own
/// timestamp — a queue UI ordering or labelling by it will show the older
/// time. That is the right of the two available lies: the entry has been
/// waiting since that moment, and re-stamping it would let a stream of folds
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
        // #615 F3: preserve both adjacent user intents under backpressure
        // rather than evicting the older send. Capped so the per-tail size
        // cannot grow unboundedly; once the cap is reached the eviction
        // fallback in `enqueue_pending_observation` drops a non-hard-fire
        // entry and lets the new message take a fresh slot. Replacing would
        // lose earlier intent; separate entries surface as separate
        // `User says:` blocks at turn issuance.
        (QueueEntry::User { text, rev, .. }, QueueEntry::User { text: new_text, .. }) => {
            if fold_user_text(text, new_text, max_folded_user_chars) {
                *rev = rev.saturating_add(1);
                true
            } else {
                false
            }
        }
        (QueueEntry::LegacyUser { text, .. }, QueueEntry::User { text: new_text, .. }) => {
            // Text is appended, but no id is minted and none is assigned: a
            // legacy entry never becomes addressable. The ack degrades to
            // `None`, which the client already handles (that is also what a
            // dormant harness returns).
            fold_user_text(text, new_text, max_folded_user_chars)
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
        (
            QueueEntry::System {
                observation:
                    Observation::ReportEdited {
                        track_id,
                        body_sha256,
                        body,
                        author,
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
                    },
                ..
            },
        ) if track_id == new_track_id => {
            *body_sha256 = new_body_sha256.clone();
            *body = new_body.clone();
            // The fold keeps the NEWEST edit's state, attribution included:
            // the planner is told to treat the surviving body as ground truth,
            // so it must be told who actually wrote that body (#1252 F2).
            *author = *new_author;
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
    // #1449 — a fold turns two entries into one, so the survivor carries BOTH
    // sets of message ids. The envelope id ADVANCES to the newest send while
    // these UNION, because they answer different questions: one is "which push
    // am I acknowledging", the other is "which instances am I still holding",
    // and a fold is still holding both. Overwriting would discard an instance
    // the give-back may later have to move back.
    let incoming_message_ids = incoming.message_ids().to_vec();
    survivor.message_ids_mut().extend(incoming_message_ids);
    FoldOutcome::Folded {
        entry_id: survivor.id().cloned(),
    }
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
        QueueEntry::user_message(text.to_string(), None)
    }

    #[test]
    fn pending_entry_system_refuses_user_message() {
        // MJ-3 / §11.3 #13. `Result` gives no compile-time signal — every
        // caller already handles one — so without this test "construction
        // failure is visible" would be a claim with no carrier. The mutation
        // that reddens it is turning this arm into `Ok(System { .. })`.
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
        let mut queue = VecDeque::from(vec![QueueEntry::user_message("first".into(), Some(1))]);
        let survivor_id = queue[0].id().cloned().expect("user entry has an id");
        let incoming = QueueEntry::user_message("second".into(), Some(2));
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
        // §11.1 #3b. The alternative — handing the legacy entry the incoming
        // id — is exactly what GAP-B's universal sentence forbids.
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

    /// #1449 — a fold must keep BOTH instances identifiable; dropping one is
    /// the loss of identity the message ids exist to prevent.
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

    /// A legacy entry gains a MESSAGE id when it moves, and still no
    /// `QueueEntryId`.
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

    /// A system entry is never moved back, so it must not acquire an identity
    /// that would suggest it could be.
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
}
