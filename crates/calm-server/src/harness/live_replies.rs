//! The reply text a Planner turn is streaming before its `item/completed` row stores it (#1923 S2).
//!
//! The server has one [`LiveReplies`], owned by the
//! [`HarnessRegistry`](crate::harness::HarnessRegistry), and `GET /api/cards/{id}/harness/live`
//! reads it. Each harness's run loop holds the one [`LiveReplyWriter`] for its card, so the run
//! loop is the only writer. Nothing here touches the database: a delta costs no write.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use calm_types::harness::{HarnessLiveReplies, HarnessLiveReply};

use crate::ids::CardId;

/// Live reply text, keyed by card.
pub struct LiveReplies {
    cards: Mutex<HashMap<CardId, CardEntry>>,
    next_writer: AtomicU64,
}

/// One card's live state, owned by the writer that opened it.
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
    items: Vec<HarnessLiveReply>,
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
                    turn.items.clone()
                },
            },
            None => HarnessLiveReplies {
                turn_id: None,
                items: Vec::new(),
            },
        }
    }

    /// Start a harness's live state for `card_id`, dropping whatever an earlier harness of the card
    /// left. Only the returned writer changes the entry from here on, and dropping it removes it.
    pub(super) fn open(
        self: &Arc<Self>,
        card_id: &CardId,
        worker_session_id: &str,
    ) -> LiveReplyWriter {
        let writer = self.next_writer.fetch_add(1, Ordering::Relaxed);
        let replaced = self.lock().insert(
            card_id.clone(),
            CardEntry {
                writer,
                worker_session_id: worker_session_id.to_owned(),
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
                %card_id,
                worker_session_id,
                previous_worker_session_id = %previous,
                turn_id = %turn.turn_id,
                "planner live replies: a new harness drops the previous harness's live turn"
            );
        }
        LiveReplyWriter {
            replies: Arc::clone(self),
            card_id: card_id.clone(),
            writer,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CardId, CardEntry>> {
        self.cards.lock().expect("live replies mutex poisoned")
    }
}

/// One run loop's handle on its card's entry. A writer whose entry a later harness of the card
/// replaced changes nothing.
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
                    items: Vec::new(),
                });
            }
        });
    }

    /// An observed reply start opens the reply, if it belongs to the active turn.
    pub(crate) fn reply_started(&self, turn_id: &str, item_id: &str) {
        self.with_turn(|turn| {
            if let Some(turn) = turn
                .as_mut()
                .filter(|turn| turn.turn_id == turn_id && !turn.lost)
                && !turn.items.iter().any(|item| item.item_id == item_id)
            {
                turn.items.push(HarnessLiveReply {
                    item_id: item_id.to_owned(),
                    text: String::new(),
                });
            }
        });
    }

    /// Append to an open reply of the active turn; any other delta is ignored.
    pub(crate) fn delta(&self, turn_id: &str, item_id: &str, delta: &str) {
        self.with_turn(|turn| {
            if let Some(turn) = turn
                .as_mut()
                .filter(|turn| turn.turn_id == turn_id && !turn.lost)
                && let Some(item) = turn.items.iter_mut().find(|item| item.item_id == item_id)
            {
                item.text.push_str(delta);
            }
        });
    }

    /// The item's `item/completed` row is stored, so its text is durable and leaves the live state.
    pub(crate) fn item_stored(&self, item_id: &str) {
        self.with_turn(|turn| {
            if let Some(turn) = turn.as_mut() {
                turn.items.retain(|item| item.item_id != item_id);
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

    /// `turn_id` ended: clear it and hand back its still-open replies. A blocked turn hands back
    /// none.
    pub(crate) fn settle(&self, turn_id: &str) -> Vec<HarnessLiveReply> {
        self.with_turn(|turn| match turn.take_if(|turn| turn.turn_id == turn_id) {
            Some(settled) if !settled.lost => settled.items,
            _ => Vec::new(),
        })
        .unwrap_or_default()
    }
}

impl Drop for LiveReplyWriter {
    /// The run loop ended: its card has no live state until the next harness opens one.
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
