//! Per-card event cursor cache used by PR8's `wait_for_events` MCP tool
//! and the `/internal/codex/pending_events` HTTP fallback.
//!
//! ## What it solves
//!
//! Both code paths are long-poll APIs the codex daemon calls between
//! turns: "tell me what's happened on my wave since my last call". The
//! caller's reasonable default for `since` is "advance from where I
//! left off last", but the caller is the codex daemon — short-lived
//! per Stop hook, no place to durably remember a cursor across
//! invocations. The kernel knows the bound card identity, so it
//! tracks the cursor server-side, keyed on `CardId`.
//!
//! ## Semantics
//!
//! The cursor is the highest `events.id` the kernel has handed back to
//! a wait/pending call for that card. A subsequent call with `since`
//! omitted defaults to this value, so the caller never sees the same
//! event id twice. Callers can also pass an explicit `since` to
//! re-replay from an earlier point (the cache update only happens when
//! we *return* events with higher ids — re-replay is non-destructive).
//!
//! ## What it is NOT
//!
//! It is not durable across server restarts. A restart drops the
//! cache, and the next call starts at id 0 (catch-up returns the
//! entire scoped history up to the per-call `limit`). This is fine for
//! the Stop-hook use case: the spec daemon's prior decisions are
//! already encoded in the wave's persisted state, and replaying old
//! task lifecycle events is idempotent (the spec sees them, decides
//! they were already handled, moves on). Durability would require a
//! new column on `cards` and a write per pending-events call, which
//! isn't worth the cost for a soft optimization.
//!
//! ## Concurrency
//!
//! `DashMap` per-key locking. Two concurrent wait calls for the same
//! card race to the higher cursor; whichever ran last wins. That race
//! is benign: both callers see the events they were given, the cache
//! converges on the winner's max id, and the next call starts cleanly
//! from there.

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
    /// the wait/pending handlers treat `0` as "send the full scoped
    /// history up to `limit`".
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

    /// Force-set the cursor (test helper). Production code uses [`bump`]
    /// so concurrent waits never rewind each other.
    #[cfg(test)]
    pub(crate) fn set(&self, card: CardId, id: i64) {
        self.inner.insert(card, id);
    }

    /// Drop a card's entry. Currently exercised only by the unit tests
    /// — the card-delete path doesn't yet thread this cache through, so
    /// stale entries linger until the next server restart. That's
    /// harmless: the cursor is a soft optimization (see the "What it is
    /// NOT" section above), a deleted card is unreachable from the
    /// scope filter, and a future caller with the same id (collisions
    /// notwithstanding) would `bump` past whatever stale value we held.
    /// Kept on the surface so a future wire-up is a one-line change.
    /// Safe on missing keys.
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
    fn set_test_helper_overrides() {
        // The test-only `set` helper does NOT check monotonicity — it's
        // for fixture setup, where tests explicitly want to plant a
        // specific value.
        let c = EventCursorCache::new();
        c.bump(cid("a"), 100);
        c.set(cid("a"), 5);
        assert_eq!(c.get(&cid("a")), 5);
    }
}
