//! Per-card event watermark cache.
//!
//! ## What it's for now (#293 cutover)
//!
//! The pull machinery that originally owned this cache
//! (`calm.wait_for_events` + the `/internal/codex/pending_events`
//! long-poll) was deleted in the #293 push cutover. The sole remaining
//! consumer is the dispatcher's **push watermark** (`Inner.push_cursor`
//! in `dispatcher.rs`): keyed by the spec `CardId`, it dedups pushed
//! observations so a re-delivered broadcast envelope (at-least-once
//! delivery) doesn't issue a duplicate `turn/start`. A push fires only
//! when `envelope_id > cursor`, then bumps.
//!
//! ## Semantics
//!
//! The cursor is the highest `events.id` already acted on for that card.
//! [`EventCursorCache::bump`] is monotonic, so an out-of-order / lower id
//! never rewinds it.
//!
//! ## Durability
//!
//! The cache itself is in-memory only. The dispatcher's PRODUCTION push
//! path mirrors the cursor onto the spec card's `payload.push_watermark`
//! (see `spec_card_set_push_watermark`) after a successful delivery, so
//! a kernel restart can seed the cache from disk via
//! [`crate::dispatcher::Dispatcher::seed_push_cursor`] and resume catch-up
//! from the last acked envelope (#313 problem #1). The in-memory cache
//! and the persisted watermark serve different roles:
//!   * **in-memory cursor** — per-process dedup hint, bumped before a
//!     push attempt so a redelivery within the same process is dropped
//!     even if delivery is in flight.
//!   * **persisted watermark** — durable floor for cross-restart catch-up;
//!     advanced ONLY on successful delivery so a crash mid-push replays
//!     the envelope on the next boot.
//!
//! ## Concurrency
//!
//! `DashMap` per-key locking. The dispatcher additionally serializes the
//! `(get → compare → bump → push)` sequence per-wave (see
//! `Inner.push_locks`) so the read-modify-write is atomic against other
//! same-wave pushes.

use crate::ids::CardId;
use dashmap::DashMap;
use std::sync::Arc;

/// Concurrent `CardId -> events.id` map. `Clone` is cheap (`Arc<DashMap<…>>`).
#[derive(Clone, Default, Debug)]
pub struct EventCursorCache {
    inner: Arc<DashMap<CardId, i64>>,
}

impl EventCursorCache {
    /// Fresh empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current cursor for `card`. Returns `0` when no entry exists —
    /// the dispatcher's push path treats any positive `envelope_id` as
    /// newer than the initial `0`, so the first push always fires.
    pub fn get(&self, card: &CardId) -> i64 {
        self.inner.get(card).map(|v| *v).unwrap_or(0)
    }

    /// Bump the cursor to `id` *only if* it's strictly higher than the
    /// current value. Defends against an out-of-order completion (two
    /// concurrent waits returning in reverse id order) accidentally
    /// rewinding the cursor. Returns the new effective value.
    pub fn bump(&self, card: CardId, id: i64) -> i64 {
        // DashMap's `entry` API gives us a single-shot
        // get-or-insert-and-update path.
        let mut entry = self.inner.entry(card).or_insert(0);
        if id > *entry {
            *entry = id;
        }
        *entry
    }

    /// Force-set the cursor. Most production paths use [`bump`] so
    /// concurrent pushes never rewind each other; runtime app-server
    /// recovery deliberately rewinds this in-process cache to the durable
    /// watermark before replaying catch-up rows.
    pub fn set(&self, card: CardId, id: i64) {
        self.inner.insert(card, id);
    }

    /// Drop a card's entry. Currently exercised only by the unit tests
    /// — the card-delete path doesn't yet thread this cache through, so
    /// stale entries linger until the next server restart. That's
    /// harmless: the cursor is a soft dedup watermark, a deleted card is
    /// unreachable from the push path, and a future caller with the same
    /// id (collisions notwithstanding) would `bump` past whatever stale
    /// value we held. Kept on the surface so a future wire-up is a
    /// one-line change. Safe on missing keys.
    pub fn remove(&self, card: &CardId) {
        self.inner.remove(card);
    }

    /// Number of entries (telemetry / test convenience).
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Mirrors `Vec::is_empty`; clippy nags otherwise.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid(s: &str) -> CardId {
        CardId::from(s)
    }

    #[test]
    fn empty_cache_returns_zero() {
        let c = EventCursorCache::new();
        assert_eq!(c.get(&cid("missing")), 0);
        assert!(c.is_empty());
    }

    #[test]
    fn bump_monotonic_set_only_increases() {
        let c = EventCursorCache::new();
        assert_eq!(c.bump(cid("a"), 10), 10);
        assert_eq!(c.get(&cid("a")), 10);
        // Lower id does not rewind.
        assert_eq!(c.bump(cid("a"), 5), 10);
        assert_eq!(c.get(&cid("a")), 10);
        // Higher id advances.
        assert_eq!(c.bump(cid("a"), 42), 42);
        assert_eq!(c.get(&cid("a")), 42);
    }

    #[test]
    fn remove_clears_entry() {
        let c = EventCursorCache::new();
        c.bump(cid("a"), 10);
        assert_eq!(c.len(), 1);
        c.remove(&cid("a"));
        assert!(c.is_empty());
        // Removing missing key is a no-op.
        c.remove(&cid("missing"));
        assert_eq!(c.get(&cid("a")), 0);
    }

    #[test]
    fn clone_shares_inner_state() {
        let a = EventCursorCache::new();
        let b = a.clone();
        a.bump(cid("x"), 7);
        assert_eq!(b.get(&cid("x")), 7);
    }

    #[test]
    fn set_overrides_cursor_value() {
        // `set` does NOT check monotonicity; production recovery uses it
        // to rewind the in-process cursor to the durable watermark.
        let c = EventCursorCache::new();
        c.bump(cid("a"), 100);
        c.set(cid("a"), 5);
        assert_eq!(c.get(&cid("a")), 5);
    }
}
