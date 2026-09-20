//! Per-card event watermark cache: the highest `events.id` already acted on for a card.
//! In-memory only; the dispatcher serializes `(get → compare → bump → push)` per track.

use crate::ids::CardId;
use dashmap::DashMap;
use std::sync::Arc;

/// Concurrent `CardId -> events.id` map. `Clone` is cheap (`Arc<DashMap<…>>`).
#[derive(Clone, Default, Debug)]
pub struct EventCursorCache {
    inner: Arc<DashMap<CardId, i64>>,
}

impl EventCursorCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `0` when no entry exists, so the first push always fires.
    pub fn get(&self, card: &CardId) -> i64 {
        self.inner.get(card).map(|v| *v).unwrap_or(0)
    }

    /// Bump only if `id` is strictly higher, so an out-of-order completion never rewinds the cursor.
    pub fn bump(&self, card: CardId, id: i64) -> i64 {
        let mut entry = self.inner.entry(card).or_insert(0);
        if id > *entry {
            *entry = id;
        }
        *entry
    }

    /// Force-set the cursor (no monotonicity check); recovery rewinds to the durable watermark with it.
    pub fn set(&self, card: CardId, id: i64) {
        self.inner.insert(card, id);
    }

    /// Drop a card's entry. Safe on missing keys.
    pub fn remove(&self, card: &CardId) {
        self.inner.remove(card);
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

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
        let c = EventCursorCache::new();
        c.bump(cid("a"), 100);
        c.set(cid("a"), 5);
        assert_eq!(c.get(&cid("a")), 5);
    }
}
