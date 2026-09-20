//! Event bus + envelope shapes. Writes go through `Repo::write_with_event`,
//! which persists the event and broadcasts only after commit, so every
//! broadcast is backed by a persisted row.

use crate::ids::ActorId;
use tokio::sync::broadcast;

// Source definitions live in calm-types; do NOT re-declare them here.
pub use calm_types::event::{
    ArtifactRef, EditAuthor, Event, EventMetadata, EventScope, SYNC_EVENT_VERSION,
    TrackUpdatedPayload, topics,
};

/// Capacity of the broadcast channel; a subscriber lagging past it gets
/// `Lagged` and is dropped, and is expected to reconnect and re-fetch.
const BUS_CAPACITY: usize = 1024;

/// What the broadcast channel carries. Not `Serialize`: the wire JSON is
/// hand-rolled in `ws::events::handle` (it splices `_id` into the flat
/// `{ev, data}` shape), and `actor` is not part of the public wire format.
#[derive(Clone, Debug)]
pub struct BroadcastEnvelope {
    /// Assigned `events.id`. `0` is never produced by the auto-increment and marks
    /// "no persisted row"; an emitter bypassing the wrapper shows as `_id: 0`.
    pub id: i64,
    /// Mirrors the `event_version` column; replay-path envelopes carry the value
    /// read back from the row.
    pub event_version: u32,
    /// Typed producer identity, persisted to `events.actor` as JSON.
    pub actor: ActorId,
    /// "Home scope"; replay-path envelopes for rows with NULL `scope_*` columns
    /// fall back to `EventScope::System`.
    pub scope: EventScope,
    pub event: Event,
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<BroadcastEnvelope>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    /// Broadcast an already-persisted event with its assigned id. Handlers should
    /// go through `write_with_event` instead so the event lands in the log.
    pub fn emit_envelope(&self, env: BroadcastEnvelope) {
        let _ = self.tx.send(env);
    }

    /// Synthetic broadcast for test scaffolding and FSM injection — `id` is `0`
    /// (no persisted row). **Production code must not call this.** Not
    /// `#[cfg(test)]` because integration tests link the library normally.
    pub fn emit(&self, actor: ActorId, ev: Event) {
        let _ = self.tx.send(BroadcastEnvelope {
            id: 0,
            event_version: SYNC_EVENT_VERSION,
            actor,
            scope: EventScope::System,
            event: ev,
        });
    }

    /// Test-only re-broadcast of a fully-formed envelope, reproducing "the SAME
    /// id delivered twice", which the production emit paths cannot. Production
    /// code must never call this — it bypasses persistence.
    #[doc(hidden)]
    pub fn emit_envelope_for_test(&self, env: BroadcastEnvelope) {
        let _ = self.tx.send(env);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BroadcastEnvelope> {
        self.tx.subscribe()
    }

    /// Narrow-subscriber API: the raw broadcast receiver; callers run their own
    /// `recv` loop and apply [`SubscribeFilter::matches`], so `RecvError::Lagged`
    /// surfaces and each subscriber picks its own catch-up policy. Exact kind-tag
    /// match only, no globs.
    pub fn subscribe_filtered(&self) -> broadcast::Receiver<BroadcastEnvelope> {
        self.tx.subscribe()
    }
}

/// Server-internal subscription filter: a scope predicate plus an optional
/// kind predicate. The kind check runs first because it is cheap.
#[derive(Debug, Clone)]
pub struct SubscribeFilter {
    pub scope: SubscribeScope,
    /// When true, a scope predicate also matches any strictly-narrower scope
    /// (`Area(c)` matches tracks and cards under it); when false, exact equality only.
    pub include_descendants: bool,
    /// `None` accepts any kind; `Some([...])` accepts only those exact `kind_tag` strings.
    pub kinds: Option<Vec<String>>,
}

/// Distinct from [`EventScope`] because it needs wildcard variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscribeScope {
    System,
    Area(crate::ids::AreaId),
    Track(crate::ids::TrackId),
    /// Match an exact card; `include_descendants` is meaningless here (cards have
    /// no children) and only the exact card matches either way.
    Card(crate::ids::CardId),
    /// Match any track-scoped envelope; with `include_descendants = true`, also
    /// any card-scoped envelope.
    AnyTrack,
    AnyCard,
    /// Match every envelope — the `subscribe()` firehose.
    Any,
}

impl SubscribeFilter {
    /// `true` iff the caller should forward this envelope.
    pub fn matches(&self, envelope: &BroadcastEnvelope) -> bool {
        if let Some(kinds) = self.kinds.as_ref() {
            let tag = envelope.event.kind_tag();
            if !kinds.iter().any(|k| k == tag) {
                return false;
            }
        }

        match &self.scope {
            SubscribeScope::Any => true,
            SubscribeScope::System => matches!(envelope.scope, EventScope::System),
            SubscribeScope::Area(c) => {
                if self.include_descendants {
                    envelope.scope.area_id() == Some(c)
                } else {
                    matches!(&envelope.scope, EventScope::Area { area } if area == c)
                }
            }
            SubscribeScope::Track(w) => {
                if self.include_descendants {
                    envelope.scope.track_id() == Some(w)
                } else {
                    matches!(&envelope.scope, EventScope::Track { track, .. } if track == w)
                }
            }
            SubscribeScope::Card(card) => envelope.scope.card_id() == Some(card),
            SubscribeScope::AnyTrack => match &envelope.scope {
                EventScope::Track { .. } => true,
                EventScope::Card { .. } => self.include_descendants,
                _ => false,
            },
            SubscribeScope::AnyCard => matches!(&envelope.scope, EventScope::Card { .. }),
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    use crate::ids::{AreaId, CardId, TrackId};

    fn env(scope: EventScope, ev: Event) -> BroadcastEnvelope {
        BroadcastEnvelope {
            id: 1,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::User,
            scope,
            event: ev,
        }
    }

    fn card_added(card: &str, track: &str) -> Event {
        Event::CardAdded(crate::model::Card {
            id: CardId::from(card),
            track_id: TrackId::from(track),
            title: None,
            kind: "terminal".into(),
            sort: 1.0,
            payload: serde_json::Value::Null,
            runtime: None,
            deletable: true,
            created_at: 0,
            updated_at: 0,
        })
    }

    fn codex_req() -> Event {
        Event::CodexWorkerRequested {
            idempotency_key: "k".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
            agent_message: None,
        }
    }

    fn task_failed() -> Event {
        Event::TaskFailed {
            idempotency_key: "k".into(),
            reason: "boom".into(),
            details: None,
            agent_message: None,
        }
    }

    fn card_scope() -> EventScope {
        EventScope::Card {
            card: CardId::from("k"),
            track: TrackId::from("w"),
            area: AreaId::from("c"),
        }
    }
    fn track_scope() -> EventScope {
        EventScope::Track {
            track: TrackId::from("w"),
            area: AreaId::from("c"),
        }
    }
    fn area_scope() -> EventScope {
        EventScope::Area {
            area: AreaId::from("c"),
        }
    }

    #[test]
    fn any_scope_accepts_everything() {
        let f = SubscribeFilter {
            scope: SubscribeScope::Any,
            include_descendants: true,
            kinds: None,
        };
        assert!(f.matches(&env(EventScope::System, codex_req())));
        assert!(f.matches(&env(area_scope(), card_added("c1", "w"))));
        assert!(f.matches(&env(track_scope(), card_added("c1", "w"))));
        assert!(f.matches(&env(card_scope(), task_failed())));
    }

    #[test]
    fn kinds_filter_exact_match() {
        let f = SubscribeFilter {
            scope: SubscribeScope::Any,
            include_descendants: true,
            kinds: Some(vec![
                "codex.worker_requested".into(),
                "terminal.worker_requested".into(),
            ]),
        };
        assert!(f.matches(&env(EventScope::System, codex_req())));
        assert!(!f.matches(&env(EventScope::System, task_failed())));
        // Not a glob — exact match only.
        assert!(!f.matches(&env(card_scope(), card_added("k", "w"))));
    }

    #[test]
    fn kinds_none_accepts_all_kinds() {
        let f = SubscribeFilter {
            scope: SubscribeScope::Any,
            include_descendants: false,
            kinds: None,
        };
        assert!(f.matches(&env(EventScope::System, task_failed())));
        assert!(f.matches(&env(card_scope(), card_added("k", "w"))));
    }

    #[test]
    fn scope_system_matches_only_system() {
        let f = SubscribeFilter {
            scope: SubscribeScope::System,
            include_descendants: true, // ignored for System
            kinds: None,
        };
        assert!(f.matches(&env(EventScope::System, codex_req())));
        assert!(!f.matches(&env(area_scope(), codex_req())));
        assert!(!f.matches(&env(card_scope(), codex_req())));
    }

    #[test]
    fn scope_area_exact_vs_descendants() {
        let exact = SubscribeFilter {
            scope: SubscribeScope::Area(AreaId::from("c")),
            include_descendants: false,
            kinds: None,
        };
        assert!(exact.matches(&env(area_scope(), codex_req())));
        assert!(!exact.matches(&env(track_scope(), codex_req())));
        assert!(!exact.matches(&env(card_scope(), codex_req())));

        let desc = SubscribeFilter {
            scope: SubscribeScope::Area(AreaId::from("c")),
            include_descendants: true,
            kinds: None,
        };
        assert!(desc.matches(&env(area_scope(), codex_req())));
        assert!(desc.matches(&env(track_scope(), codex_req())));
        assert!(desc.matches(&env(card_scope(), codex_req())));
        let other = EventScope::Track {
            track: TrackId::from("w2"),
            area: AreaId::from("c2"),
        };
        assert!(!desc.matches(&env(other, codex_req())));
    }

    #[test]
    fn scope_track_exact_vs_descendants() {
        let exact = SubscribeFilter {
            scope: SubscribeScope::Track(TrackId::from("w")),
            include_descendants: false,
            kinds: None,
        };
        assert!(exact.matches(&env(track_scope(), codex_req())));
        assert!(!exact.matches(&env(card_scope(), codex_req())));

        let desc = SubscribeFilter {
            scope: SubscribeScope::Track(TrackId::from("w")),
            include_descendants: true,
            kinds: None,
        };
        assert!(desc.matches(&env(track_scope(), codex_req())));
        assert!(desc.matches(&env(card_scope(), codex_req())));
        assert!(!desc.matches(&env(area_scope(), codex_req())));
    }

    #[test]
    fn scope_card_only_exact() {
        let f = SubscribeFilter {
            scope: SubscribeScope::Card(CardId::from("k")),
            include_descendants: false,
            kinds: None,
        };
        assert!(f.matches(&env(card_scope(), codex_req())));
        let other = EventScope::Card {
            card: CardId::from("k2"),
            track: TrackId::from("w"),
            area: AreaId::from("c"),
        };
        assert!(!f.matches(&env(other, codex_req())));
        assert!(!f.matches(&env(track_scope(), codex_req())));
    }

    #[test]
    fn scope_anywave_with_and_without_descendants() {
        let no_desc = SubscribeFilter {
            scope: SubscribeScope::AnyTrack,
            include_descendants: false,
            kinds: None,
        };
        assert!(no_desc.matches(&env(track_scope(), codex_req())));
        assert!(!no_desc.matches(&env(card_scope(), codex_req())));
        assert!(!no_desc.matches(&env(area_scope(), codex_req())));

        let desc = SubscribeFilter {
            scope: SubscribeScope::AnyTrack,
            include_descendants: true,
            kinds: None,
        };
        assert!(desc.matches(&env(track_scope(), codex_req())));
        assert!(desc.matches(&env(card_scope(), codex_req())));
        assert!(!desc.matches(&env(area_scope(), codex_req())));
    }

    #[test]
    fn scope_anycard_matches_only_card() {
        let f = SubscribeFilter {
            scope: SubscribeScope::AnyCard,
            include_descendants: true,
            kinds: None,
        };
        assert!(f.matches(&env(card_scope(), codex_req())));
        assert!(!f.matches(&env(track_scope(), codex_req())));
        assert!(!f.matches(&env(EventScope::System, codex_req())));
    }

    #[test]
    fn kind_then_scope_short_circuit() {
        let f = SubscribeFilter {
            scope: SubscribeScope::Area(AreaId::from("c")),
            include_descendants: true,
            kinds: Some(vec!["codex.worker_requested".into()]),
        };
        assert!(!f.matches(&env(area_scope(), task_failed())));
        assert!(f.matches(&env(card_scope(), codex_req())));
    }
}
