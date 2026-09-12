//! Hook signals for Planner-opened terminals (#1620): a bounded ring of
//! application lifecycle events (Claude Code hooks forwarded by the bridge)
//! per renderer entry, plus a watch channel so a `wait_for=signal` can wake
//! without polling.
//!
//! Signals are untrusted advisory telemetry: the ingest route is loopback
//! only and keyed by card id, so any local process can forge `event` and
//! `message`. Nothing here is consulted by an input fence (binding, control
//! lease, revision, pending write, physical write authority); the ring is
//! presentation for the Planner, never authority.
use serde_json::{Value, json};
use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use tokio::sync::watch;

/// Ring capacity: older signals are dropped and reported as
/// `dropped_since_previous_observation`.
pub const SIGNAL_RING_CAPACITY: usize = 64;
/// Recent idempotency keys kept so a duplicate delivery of the same hook
/// (the ingest dedupe cache is check-then-insert across an await) never
/// appends twice.
const RECENT_KEYS_CAPACITY: usize = 128;
/// Longest `message` retained on a signal (characters, control chars removed).
pub const SIGNAL_MESSAGE_MAX_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signal {
    pub seq: u64,
    /// snake_case hook name (`stop`, `notification`, `permission_request`, ...).
    pub event: String,
    pub notification_type: Option<String>,
    /// Application text (already bounded and control-stripped). Forgeable data.
    pub message: String,
    pub claude_session_id: Option<String>,
    pub received_at_ms: i64,
}

impl Signal {
    pub fn to_json(&self) -> Value {
        json!({
            "seq": self.seq,
            "event": self.event,
            "notification_type": self.notification_type,
            "message": self.message,
            "claude_session_id": self.claude_session_id,
            "received_at_ms": self.received_at_ms,
        })
    }
}

/// A parsed hook body ready to append: everything but `seq`/`received_at_ms`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingSignal {
    pub event: String,
    pub notification_type: Option<String>,
    pub message: String,
    pub claude_session_id: Option<String>,
}

/// Signals since a baseline, read atomically with the ring's `last_seq`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalsSince {
    pub signals: Vec<Signal>,
    pub last_seq: u64,
    /// Signals with `seq > baseline` that are not in `signals` (ring overflow
    /// or the report limit).
    pub dropped: u64,
}

struct RingState {
    ring: VecDeque<Signal>,
    last_seq: u64,
    recent_keys: VecDeque<String>,
    recent_key_set: HashSet<String>,
    /// Published under the same lock as `last_seq` (same pattern as
    /// `ModelView::published`); `send_modify` retains updates with zero
    /// subscribers.
    published: watch::Sender<u64>,
}

pub struct SignalRing {
    state: Mutex<RingState>,
}

impl Default for SignalRing {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalRing {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RingState {
                ring: VecDeque::with_capacity(SIGNAL_RING_CAPACITY),
                last_seq: 0,
                recent_keys: VecDeque::with_capacity(RECENT_KEYS_CAPACITY),
                recent_key_set: HashSet::with_capacity(RECENT_KEYS_CAPACITY),
                published: watch::channel(0u64).0,
            }),
        }
    }

    /// Append one accepted hook. `idempotency_key` makes a duplicate delivery
    /// a no-op (`None`); otherwise the fresh `seq` is returned. Byte-identical
    /// bodies under different keys are distinct events.
    pub fn push(
        &self,
        idempotency_key: &str,
        incoming: IncomingSignal,
        now_ms: i64,
    ) -> Option<u64> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.recent_key_set.contains(idempotency_key) {
            return None;
        }
        if state.recent_keys.len() >= RECENT_KEYS_CAPACITY
            && let Some(evicted) = state.recent_keys.pop_front()
        {
            state.recent_key_set.remove(&evicted);
        }
        state.recent_keys.push_back(idempotency_key.to_owned());
        state.recent_key_set.insert(idempotency_key.to_owned());
        let seq = state.last_seq.checked_add(1)?;
        state.last_seq = seq;
        if state.ring.len() >= SIGNAL_RING_CAPACITY {
            state.ring.pop_front();
        }
        state.ring.push_back(Signal {
            seq,
            event: incoming.event,
            notification_type: incoming.notification_type,
            message: incoming.message,
            claude_session_id: incoming.claude_session_id,
            received_at_ms: now_ms,
        });
        state.published.send_modify(|value| *value = seq);
        Some(seq)
    }

    /// Seq notifications for signal waiting; no polling is required.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .published
            .subscribe()
    }

    /// Live signal subscribers: a signal wait counts from the moment it
    /// subscribes until it returns. Test observability of a wait in progress.
    pub fn signal_waiters(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .published
            .receiver_count()
    }

    pub fn last_seq(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_seq
    }

    /// Whether any signal was ever accepted for this terminal.
    pub fn hooks_seen(&self) -> bool {
        self.last_seq() > 0
    }

    /// The most recent `limit` signals with `seq > baseline`, together with
    /// the ring's `last_seq` read under the same lock, so a caller that
    /// advances its baseline to `last_seq` cannot skip an unseen signal.
    pub fn since(&self, baseline: u64, limit: usize) -> SignalsSince {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let available: Vec<Signal> = state
            .ring
            .iter()
            .filter(|signal| signal.seq > baseline)
            .cloned()
            .collect();
        let total = state.last_seq.saturating_sub(baseline);
        let skip = available.len().saturating_sub(limit);
        let signals: Vec<Signal> = available.into_iter().skip(skip).collect();
        let dropped = total.saturating_sub(signals.len() as u64);
        SignalsSince {
            signals,
            last_seq: state.last_seq,
            dropped,
        }
    }

    /// The earliest signal with `seq > baseline` whose event is in `events`.
    pub fn first_matching(&self, baseline: u64, events: &[String]) -> Option<Signal> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .ring
            .iter()
            .find(|signal| signal.seq > baseline && events.iter().any(|e| e == &signal.event))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incoming(event: &str) -> IncomingSignal {
        IncomingSignal {
            event: event.into(),
            notification_type: None,
            message: String::new(),
            claude_session_id: None,
        }
    }

    #[test]
    fn duplicate_key_appends_once_and_identical_bodies_get_fresh_seqs() {
        let ring = SignalRing::new();
        assert_eq!(ring.push("k1", incoming("stop"), 1), Some(1));
        assert_eq!(ring.push("k1", incoming("stop"), 2), None);
        assert_eq!(ring.push("k2", incoming("stop"), 3), Some(2));
        assert_eq!(ring.last_seq(), 2);
        assert!(ring.hooks_seen());
        let since = ring.since(0, 20);
        assert_eq!(since.signals.len(), 2);
        assert_eq!(since.dropped, 0);
        assert_eq!(since.last_seq, 2);
    }

    #[test]
    fn overflow_and_report_limit_are_counted_as_dropped() {
        let ring = SignalRing::new();
        for i in 0..(SIGNAL_RING_CAPACITY as u64 + 10) {
            ring.push(&format!("k{i}"), incoming("notification"), i as i64);
        }
        let since = ring.since(0, 20);
        assert_eq!(since.last_seq, SIGNAL_RING_CAPACITY as u64 + 10);
        assert_eq!(since.signals.len(), 20);
        // The most recent 20 are listed; everything else since the baseline
        // is reported as dropped.
        assert_eq!(since.signals.last().unwrap().seq, since.last_seq);
        assert_eq!(since.dropped, since.last_seq - 20);
        let recent = ring.since(since.last_seq - 3, 20);
        assert_eq!(recent.signals.len(), 3);
        assert_eq!(recent.dropped, 0);
        // Fully overflowed baseline: ten signals are gone from the ring.
        let old = ring.since(0, 100);
        assert_eq!(old.signals.len(), SIGNAL_RING_CAPACITY);
        assert_eq!(old.dropped, 10);
    }

    #[test]
    fn first_matching_respects_baseline_and_event_filter() {
        let ring = SignalRing::new();
        ring.push("a", incoming("user_prompt_submit"), 0);
        ring.push("b", incoming("stop"), 0);
        ring.push("c", incoming("stop"), 0);
        let stop = |baseline| ring.first_matching(baseline, &["stop".to_string()]);
        assert_eq!(stop(0).map(|s| s.seq), Some(2));
        assert_eq!(stop(2).map(|s| s.seq), Some(3));
        assert_eq!(stop(3), None);
        assert_eq!(ring.first_matching(0, &["session_end".to_string()]), None);
    }

    #[tokio::test]
    async fn push_wakes_subscribers_and_retains_the_seq_without_subscribers() {
        let ring = SignalRing::new();
        ring.push("a", incoming("stop"), 0);
        let mut rx = ring.subscribe();
        assert_eq!(*rx.borrow_and_update(), 1);
        assert_eq!(ring.signal_waiters(), 1);
        ring.push("b", incoming("stop"), 0);
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow_and_update(), 2);
    }
}
