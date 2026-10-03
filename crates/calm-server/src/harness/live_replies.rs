//! The reply text a Planner turn is streaming before its `item/completed` row stores it (#1923 S2).
//!
//! The server has one [`LiveReplies`], owned by the
//! [`HarnessRegistry`](crate::harness::HarnessRegistry), and `GET /api/cards/{id}/harness/live`
//! reads it. Each harness's run loop holds one [`LiveReplyWriter`] for its card, and only the
//! writer of the harness the registry installed changes the card's entry, so the run loop is the
//! only writer. Nothing here touches the database: a delta costs no write.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use calm_types::harness::{HarnessLiveReplies, HarnessLiveReply};
use serde_json::Value;

use crate::ids::CardId;

/// Live reply text, keyed by card.
pub struct LiveReplies {
    cards: Mutex<HashMap<CardId, CardEntry>>,
    next_writer: AtomicU64,
}

/// One card's live state, owned by the writer whose harness was installed last.
struct CardEntry {
    writer: u64,
    worker_session_id: String,
    turn: Option<LiveTurn>,
}

/// The active turn's open replies, in the order they started.
struct LiveTurn {
    turn_id: String,
    /// Set when the run loop's input lagged: the turn may have lost a delta, so none of its
    /// replies is shown or stored from here on.
    lost: bool,
    /// Set when the turn ended: it takes no more replies or text, and each reply it still holds
    /// stays until its row is stored or the next turn starts.
    settled: bool,
    items: Vec<OpenReply>,
}

/// One open reply: the item its `item/started` carried and the text streamed into it so far.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenReply {
    pub(crate) item_id: String,
    /// The `item` of the reply's `item/started` row.
    pub(crate) started: Value,
    pub(crate) text: String,
}

impl LiveTurn {
    /// The turn, if it is `turn_id` and still takes replies and text.
    fn open<'a>(turn: &'a mut Option<LiveTurn>, turn_id: &str) -> Option<&'a mut LiveTurn> {
        turn.as_mut()
            .filter(|turn| turn.turn_id == turn_id && !turn.lost && !turn.settled)
    }
}

impl LiveReplies {
    /// The [`HarnessRegistry`](crate::harness::HarnessRegistry) builds the server's one instance.
    pub(super) fn new() -> Self {
        Self {
            cards: Mutex::new(HashMap::new()),
            next_writer: AtomicU64::new(0),
        }
    }

    /// A registry of a test's own, for a harness built outside the production wiring.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn for_test() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// What `GET /api/cards/{id}/harness/live` answers for `card_id`.
    pub fn read(&self, card_id: &CardId) -> HarnessLiveReplies {
        let cards = self.lock();
        match cards.get(card_id).and_then(|entry| entry.turn.as_ref()) {
            Some(turn) => HarnessLiveReplies {
                turn_id: Some(turn.turn_id.clone()),
                items: if turn.lost {
                    Vec::new()
                } else {
                    turn.items
                        .iter()
                        .map(|item| HarnessLiveReply {
                            item_id: item.item_id.clone(),
                            text: item.text.clone(),
                        })
                        .collect()
                },
            },
            None => HarnessLiveReplies {
                turn_id: None,
                items: Vec::new(),
            },
        }
    }

    /// A new harness's claim on `card_id`'s entry. Nothing changes until the registry installs
    /// the harness, so a harness that loses its start to a concurrent one never touches the card.
    pub(crate) fn claim(
        self: &Arc<Self>,
        card_id: &CardId,
        worker_session_id: &str,
    ) -> LiveReplyClaim {
        LiveReplyClaim {
            replies: Arc::clone(self),
            card_id: card_id.clone(),
            worker_session_id: worker_session_id.to_owned(),
            writer: self.next_writer.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CardId, CardEntry>> {
        self.cards.lock().expect("live replies mutex poisoned")
    }
}

/// A harness's claim on its card's entry; the harness holds it, and its run loop the writer.
pub(crate) struct LiveReplyClaim {
    replies: Arc<LiveReplies>,
    card_id: CardId,
    worker_session_id: String,
    writer: u64,
}

impl LiveReplyClaim {
    /// The run loop's writer. It changes nothing until [`Self::install`] runs.
    pub(crate) fn writer(&self) -> LiveReplyWriter {
        LiveReplyWriter {
            replies: Arc::clone(&self.replies),
            card_id: self.card_id.clone(),
            writer: self.writer,
        }
    }

    /// The registry installed the harness: its writer owns the card's entry from here on, and
    /// whatever an earlier harness of the card left is dropped.
    pub(crate) fn install(&self) {
        let replaced = self.replies.lock().insert(
            self.card_id.clone(),
            CardEntry {
                writer: self.writer,
                worker_session_id: self.worker_session_id.clone(),
                turn: None,
            },
        );
        if let Some(CardEntry {
            worker_session_id: previous,
            turn: Some(turn),
            ..
        }) = replaced
        {
            tracing::debug!(
                card_id = %self.card_id,
                worker_session_id = %self.worker_session_id,
                previous_worker_session_id = %previous,
                turn_id = %turn.turn_id,
                "planner live replies: a new harness drops the previous harness's live turn"
            );
        }
    }
}

/// One run loop's handle on its card's entry. A writer whose harness was never installed, or
/// whose entry a later harness of the card replaced, changes nothing.
pub(crate) struct LiveReplyWriter {
    replies: Arc<LiveReplies>,
    card_id: CardId,
    writer: u64,
}

impl LiveReplyWriter {
    fn with_turn<R>(&self, change: impl FnOnce(&mut Option<LiveTurn>) -> R) -> Option<R> {
        let mut cards = self.replies.lock();
        let entry = cards
            .get_mut(&self.card_id)
            .filter(|entry| entry.writer == self.writer)?;
        Some(change(&mut entry.turn))
    }

    /// The run loop accepted `turn_id` as the running turn. Another turn's replies are dropped; the
    /// same turn seen again keeps its own.
    pub(crate) fn turn_started(&self, turn_id: &str) {
        self.with_turn(|turn| {
            if turn.as_ref().is_none_or(|turn| turn.turn_id != turn_id) {
                *turn = Some(LiveTurn {
                    turn_id: turn_id.to_owned(),
                    lost: false,
                    settled: false,
                    items: Vec::new(),
                });
            }
        });
    }

    /// An observed reply start opens the reply, if it belongs to the active turn. `started` is the
    /// start's `item`, the body a partial row of the reply stores.
    pub(crate) fn reply_started(&self, turn_id: &str, item_id: &str, started: &Value) {
        self.with_turn(|turn| {
            if let Some(turn) = LiveTurn::open(turn, turn_id)
                && !turn.items.iter().any(|item| item.item_id == item_id)
            {
                turn.items.push(OpenReply {
                    item_id: item_id.to_owned(),
                    started: started.clone(),
                    text: String::new(),
                });
            }
        });
    }

    /// Append to an open reply of the active turn; any other delta is ignored.
    pub(crate) fn delta(&self, turn_id: &str, item_id: &str, delta: &str) {
        self.with_turn(|turn| {
            if let Some(turn) = LiveTurn::open(turn, turn_id)
                && let Some(item) = turn.items.iter_mut().find(|item| item.item_id == item_id)
            {
                item.text.push_str(delta);
            }
        });
    }

    /// The item's `item/completed` row is stored, so its text is durable and leaves the live state.
    /// A settled turn whose last reply this was is cleared.
    pub(crate) fn item_stored(&self, item_id: &str) {
        self.with_turn(|turn| {
            if let Some(live) = turn.as_mut() {
                live.items.retain(|item| item.item_id != item_id);
                if live.settled && live.items.is_empty() {
                    *turn = None;
                }
            }
        });
    }

    /// The run loop's input lagged: block the whole active turn.
    pub(crate) fn input_lost(&self) {
        self.with_turn(|turn| {
            if let Some(turn) = turn.as_mut() {
                turn.lost = true;
                turn.items.clear();
            }
        });
    }

    /// `turn_id` ended with replies to store: mark it settled and hand back a copy of its open
    /// replies, each of which stays live until [`Self::item_stored`] removes it. A blocked turn, a
    /// turn with nothing open, a turn already settled and another turn hand back none.
    pub(crate) fn settle(&self, turn_id: &str) -> Vec<OpenReply> {
        self.with_turn(|turn| {
            let Some(live) = LiveTurn::open(turn, turn_id) else {
                if turn
                    .as_ref()
                    .is_some_and(|live| live.turn_id == turn_id && live.lost)
                {
                    *turn = None;
                }
                return Vec::new();
            };
            live.settled = true;
            let open = live.items.clone();
            if open.is_empty() {
                *turn = None;
            }
            open
        })
        .unwrap_or_default()
    }

    /// `turn_id` ended with nothing to store: clear it.
    pub(crate) fn discard(&self, turn_id: &str) {
        self.with_turn(|turn| {
            turn.take_if(|turn| turn.turn_id == turn_id);
        });
    }
}

impl Drop for LiveReplyWriter {
    /// The run loop ended: its card has no live state until the next harness is installed.
    fn drop(&mut self) {
        let mut cards = self.replies.lock();
        if cards
            .get(&self.card_id)
            .is_some_and(|entry| entry.writer == self.writer)
        {
            cards.remove(&self.card_id);
        }
    }
}
