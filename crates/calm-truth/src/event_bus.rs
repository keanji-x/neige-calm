//! Event bus + envelope shapes.
//!
//! ## Sync engine phase 1 (Scope A) overview
//!
//! Every write that mutates kernel-owned state flows through
//! `Repo::write_with_event` (see `db::mod`). That wrapper:
//!
//!   1. Opens a single sqlx transaction.
//!   2. Runs the caller-supplied closure (entity inserts / updates).
//!   3. Persists the produced `Event` into the `events` table (sync engine
//!      log) inside the same transaction.
//!   4. Commits, then — and **only** then — emits the event onto the
//!      `EventBus` wrapped in a `BroadcastEnvelope { id, actor, event }`.
//!
//! The wrapper guarantees the *commit-then-emit* invariant: if the txn
//! rolls back, neither the entity row nor the event row exists, and the
//! broadcast never fires. Conversely, a successful broadcast is always
//! backed by a persisted event row, which the eventual Scope D replay
//! protocol relies on.
//!
//! ## #679 PR1 split — vocabulary vs transport
//!
//! The typed [`Event`] enum, its payload data types, [`EventScope`],
//! [`SYNC_EVENT_VERSION`], [`EventMetadata`] and [`topics`] moved to
//! `calm_types::event` (they are wire *shape*, ts-rs source included) and
//! are re-exported here so every `crate::event::Event` path is unchanged.
//! This module keeps the *transport*: [`EventBus`] / [`BroadcastEnvelope`]
//! (tokio broadcast — IO, stays above the calm-types firewall) and the
//! kernel-internal [`SubscribeFilter`].
//!
//! ## Why `BroadcastEnvelope`, not `Event::id`
//!
//! The wire format must carry the assigned event id (`_id` field, per
//! design §2.4) so clients can advance their cursor. We pass that id over
//! the broadcast channel rather than baking it into every `Event` variant
//! because:
//!
//!   * the typed `Event` enum is the **ts-rs** source for the frontend;
//!     adding `id` would force every variant to thread it through (and
//!     change every `Event::CardAdded(card)` construction site to also
//!     carry an id that the producer didn't yet know);
//!   * `_id` is a transport-layer envelope concern, not a domain concern
//!     — same reason `ev` and `data` live on the envelope and not on the
//!     event payloads themselves.
//!
//! The WS `/api/events` handler unwraps the envelope, serializes the
//! `Event` (`{ "ev": ..., "data": ... }`), then injects `"_id": <id>` into
//! the resulting JSON object before sending it down the wire. See
//! `ws::events::handle`.

use crate::ids::ActorId;
use tokio::sync::broadcast;

// #679 PR1 — moved vocabulary, re-exported at the old paths. Source
// definitions live in calm-types; do NOT re-declare them here.
pub use calm_types::event::{
    ArtifactRef, EditAuthor, Event, EventMetadata, EventScope, SYNC_EVENT_VERSION,
    WaveUpdatedPayload, topics,
};

/// Capacity of the broadcast channel. If a subscriber lags more than this,
/// it'll receive a `Lagged` error and the server drops its connection — the
/// client is expected to reconnect and re-fetch (and once Scope D lands,
/// resume via `since=<lastId>`).
const BUS_CAPACITY: usize = 1024;

/// What the broadcast channel actually carries. The `id` is the row id
/// returned by `events.id`'s AUTOINCREMENT insert (see `Repo::write_with_event`
/// and `Repo::log_pure_event`). The WS handler uses it to stamp `_id` on the
/// outgoing JSON envelope per design doc §2.4.
///
/// We don't derive `Serialize` here — the serialization of the envelope into
/// the wire JSON is hand-rolled in `ws::events::handle` (it has to splice
/// `_id` alongside the existing `{ev, data}` flat shape rather than nest
/// `event` as a sub-object). `actor` is not part of the public wire format
/// either (see `ws::events::render_envelope`); it lives on the envelope so
/// in-process subscribers (today: the `RECORD_SESSION` recorder) can capture
/// attribution that the persisted `events.actor` column carries.
#[derive(Clone, Debug)]
pub struct BroadcastEnvelope {
    /// Assigned `events.id`. `0` is reserved (never produced by the
    /// auto-increment), used here as a sentinel for "no persisted row" in
    /// out-of-scope code paths that haven't yet been migrated to
    /// `write_with_event` / `log_pure_event`. Scope A converts every site
    /// the design doc names; any future emitter that bypasses the wrapper
    /// will surface as `_id: 0` on the wire, which is a useful canary.
    pub id: i64,
    /// Sync-engine envelope version stamp. Mirrors the `event_version`
    /// column on the persisted `events` row (migration 0006). Always set
    /// to `SYNC_EVENT_VERSION` for fresh writes; replay-path envelopes
    /// carry the value read back from the row (old rows backfill to `1`
    /// via the column default). Surfaced on the WS frame as `eventVersion`.
    pub event_version: u32,
    /// Typed producer identity. Persisted to the `events.actor` TEXT column
    /// as `serde_json::to_string(&actor)` so future actor variants (e.g.
    /// `ActorId::Plugin { id, version }`) round-trip without a schema bump.
    /// Used by `replay::spawn_session_recorder` so `RECORD_SESSION` traces
    /// preserve real attribution.
    pub actor: ActorId,
    /// "Home scope" — which cove/wave/card this event belongs to. PR3+
    /// filter and route on this without re-parsing the event payload.
    /// Replay-path envelopes for pre-PR2 rows (NULL `scope_*` columns)
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

    /// Internal helper used by the `Repo::write_with_event` /
    /// `Repo::log_pure_event` wrappers to broadcast an already-persisted
    /// event with its assigned id. Returns silently if there are no current
    /// subscribers.
    ///
    /// Direct callers outside the repo wrappers should not exist; if you
    /// find yourself reaching for this from a handler, you almost
    /// certainly want to go through `write_with_event` instead so the
    /// event lands in the persistent log.
    pub fn emit_envelope(&self, env: BroadcastEnvelope) {
        let _ = self.tx.send(env);
    }

    /// Synthetic broadcast for test scaffolding and FSM injection — emits
    /// the event with an `id` of `0` (no persisted row).
    ///
    /// **Production code must not call this.** Production writes must
    /// flow through `Repo::write_with_event` or `Repo::log_pure_event`
    /// so the broadcast carries a real `events.id`. The `grep` lint
    /// guards (`grep -rn "events.emit" crates/calm-server/src/{routes,plugin_host}`)
    /// must return zero hits for production code; only tests and the
    /// internal `card_fsm` test injection use this.
    ///
    /// `actor` is the declared producer identity (PR2 of #136 typed this
    /// — pass `ActorId::User` / `ActorId::Kernel` / `ActorId::Plugin(...)`
    /// matching the production code path you're stand-in for). `scope`
    /// defaults to `EventScope::System` to keep the test ergonomics —
    /// tests that need to exercise scope-aware filtering should call
    /// `emit_envelope` directly.
    ///
    /// Available outside `#[cfg(test)]` because integration tests in
    /// `crates/calm-server/tests/` consume the library through normal
    /// linkage — they don't see `#[cfg(test)]`-gated items.
    pub fn emit(&self, actor: ActorId, ev: Event) {
        let _ = self.tx.send(BroadcastEnvelope {
            id: 0,
            event_version: SYNC_EVENT_VERSION,
            actor,
            scope: EventScope::System,
            event: ev,
        });
    }

    /// Test-only re-broadcast of a fully-formed [`BroadcastEnvelope`]
    /// (explicit `id` + `scope`), mirroring the at-least-once redelivery a
    /// reconnecting subscriber would see off the broadcast channel.
    ///
    /// The production emit paths (`write_with_event` / `log_pure_event`)
    /// assign a fresh strictly-increasing `events.id` each call, so they
    /// can't reproduce "the SAME id delivered twice". The #293 PR3b
    /// dispatcher dedups its spec-push on `envelope.id`; its gated e2e uses
    /// this helper to redeliver a previously-persisted envelope verbatim and
    /// assert the second delivery does NOT double-push.
    ///
    /// `#[doc(hidden)]` to keep it off the public docs, and available
    /// outside `#[cfg(test)]` for the same reason [`emit`](Self::emit) is:
    /// integration tests link the library normally and don't see
    /// `#[cfg(test)]`-gated items. ([`emit`](Self::emit) itself is NOT
    /// `#[doc(hidden)]`; only the "available outside `#[cfg(test)]`" rationale
    /// is shared.) Production code must never call this — it bypasses
    /// persistence.
    #[doc(hidden)]
    pub fn emit_envelope_for_test(&self, env: BroadcastEnvelope) {
        let _ = self.tx.send(env);
    }

    /// New subscriber. The receiver picks up envelopes emitted after this
    /// call.
    pub fn subscribe(&self) -> broadcast::Receiver<BroadcastEnvelope> {
        self.tx.subscribe()
    }

    /// PR5 of #136 — narrow-subscriber API. The returned receiver is the
    /// raw broadcast receiver; callers run their own `recv` loop and
    /// invoke [`SubscribeFilter::matches`] against each envelope. This
    /// keeps the API dependency-free (no `tokio-stream` / `BroadcastStream`
    /// wrapper) and surfaces `RecvError::Lagged` explicitly so each
    /// subscriber can decide its own catch-up policy — the dispatcher,
    /// for instance, treats a lag as a missed event whose next
    /// `*.Requested` emit re-triggers the idempotency check, so it just
    /// logs at `warn` and continues.
    ///
    /// The filter itself is server-internal — no wire format, no schema
    /// cost. Plugins still subscribe through the WS `topics()` /
    /// `plugin_host::events` filter API; `SubscribeFilter` is for the
    /// dispatcher (PR5) and any future kernel-internal worker that needs a
    /// per-`EventScope` / per-`kind` cut of the firehose.
    ///
    /// **Glob support for `kinds` is out of scope for PR5** — exact
    /// kind-tag match only. A future extension can add prefix globs
    /// (`"task.*"`) by widening [`SubscribeFilter::kinds`] semantics.
    /// The dispatcher subscribes with explicit kind list
    /// `["codex.worker_requested", "terminal.worker_requested"]`.
    ///
    /// Relationship to [`topics`]: `topics()` is the plugin-host /
    /// WS-client filter grammar (`"card:<id>"`, `"plugin:*"`, glob over
    /// event names). It runs *after* `SubscribeFilter` — i.e. plugins
    /// always see the full firehose through `subscribe()` and then the
    /// plugin host narrows via the topics dictionary. `SubscribeFilter`
    /// is the parallel API for in-process workers that don't want to
    /// pay the cost of running every event through their match logic.
    pub fn subscribe_filtered(&self) -> broadcast::Receiver<BroadcastEnvelope> {
        self.tx.subscribe()
    }
}

// ---------------------------------------------------------------------------
// SubscribeFilter (PR5 of #136)
// ---------------------------------------------------------------------------

/// Server-internal subscription filter. PR5 of #136 lands the type +
/// matching logic; the dispatcher (`crate::dispatcher`) is the only
/// consumer today.
///
/// The filter combines a scope predicate (where in the cove→wave→card
/// tree we care about) with an optional kind predicate (which event
/// variants we care about). The kind check runs first because it's
/// cheap (single string compare against the persisted kind tag).
///
/// See [`EventBus::subscribe_filtered`] for the receiver API. Callers
/// own the loop — `matches()` is the only per-envelope work this type
/// exposes.
#[derive(Debug, Clone)]
pub struct SubscribeFilter {
    pub scope: SubscribeScope,
    /// When true, a scope predicate matches *that scope and any
    /// strictly-narrower scope* (e.g. `Cove(c)` with `descendants =
    /// true` matches `Cove{c}`, `Wave{cove=c,...}`, and
    /// `Card{cove=c,...}`). When false, only exact equality matches —
    /// e.g. `Cove(c)` matches the cove-level event but not any wave
    /// under it. The dispatcher uses `true` so a `*.worker_requested`
    /// emitted from any spec card scope (Card) routes upward.
    pub include_descendants: bool,
    /// `None` accepts any kind; `Some([...])` accepts only those exact
    /// `kind_tag` strings. No glob support in PR5 — see
    /// [`EventBus::subscribe_filtered`] docs for the extension story.
    pub kinds: Option<Vec<String>>,
}

/// What part of the cove→wave→card tree (and which envelopes-without-
/// a-tree-position) a [`SubscribeFilter`] cares about. Distinct from
/// [`EventScope`] because we need wildcard variants (`AnyWave`,
/// `AnyCard`, `Any`) the persisted-event type doesn't need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscribeScope {
    /// Match envelopes with `EventScope::System` exactly.
    System,
    /// Match an exact cove (or any wave/card under it when
    /// `include_descendants = true`).
    Cove(crate::ids::CoveId),
    /// Match an exact wave (or any card under it when
    /// `include_descendants = true`).
    Wave(crate::ids::WaveId),
    /// Match an exact card. `include_descendants` is meaningless here
    /// (cards have no children in the sync-engine hierarchy) but the
    /// filter still honors the flag uniformly — true or false, only
    /// the exact card matches.
    Card(crate::ids::CardId),
    /// Match any wave-scoped envelope. With `include_descendants =
    /// true`, also matches any card-scoped envelope (a card scope is a
    /// descendant of *some* wave).
    AnyWave,
    /// Match any card-scoped envelope.
    AnyCard,
    /// Match every envelope — equivalent to today's `subscribe()`
    /// firehose. The dispatcher uses this because its kinds list
    /// already narrows to two variants.
    Any,
}

impl SubscribeFilter {
    /// Test an envelope against the filter. Returns `true` iff the
    /// caller should forward this envelope.
    ///
    /// Order:
    ///   1. Kind check (cheap — single string compare against the
    ///      cached `kind_tag()`).
    ///   2. Scope check against `envelope.scope`, honoring
    ///      `include_descendants`.
    pub fn matches(&self, envelope: &BroadcastEnvelope) -> bool {
        // 1. Kind predicate. `None` is "accept any kind".
        if let Some(kinds) = self.kinds.as_ref() {
            let tag = envelope.event.kind_tag();
            if !kinds.iter().any(|k| k == tag) {
                return false;
            }
        }

        // 2. Scope predicate. Each variant of SubscribeScope decides
        //    its own match logic; `include_descendants` widens the
        //    Cove/Wave variants to also accept narrower scopes.
        match &self.scope {
            SubscribeScope::Any => true,
            SubscribeScope::System => matches!(envelope.scope, EventScope::System),
            SubscribeScope::Cove(c) => {
                if self.include_descendants {
                    envelope.scope.cove_id() == Some(c)
                } else {
                    matches!(&envelope.scope, EventScope::Cove { cove } if cove == c)
                }
            }
            SubscribeScope::Wave(w) => {
                if self.include_descendants {
                    envelope.scope.wave_id() == Some(w)
                } else {
                    matches!(&envelope.scope, EventScope::Wave { wave, .. } if wave == w)
                }
            }
            SubscribeScope::Card(card) => envelope.scope.card_id() == Some(card),
            SubscribeScope::AnyWave => match &envelope.scope {
                EventScope::Wave { .. } => true,
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

// ---------------------------------------------------------------------------
// SubscribeFilter unit tests (PR5 of #136)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod filter_tests {
    use super::*;
    use crate::ids::{CardId, CoveId, WaveId};

    fn env(scope: EventScope, ev: Event) -> BroadcastEnvelope {
        BroadcastEnvelope {
            id: 1,
            event_version: SYNC_EVENT_VERSION,
            actor: ActorId::User,
            scope,
            event: ev,
        }
    }

    fn card_added(card: &str, wave: &str) -> Event {
        Event::CardAdded(crate::model::Card {
            id: CardId::from(card),
            wave_id: WaveId::from(wave),
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
            agent_message: None,
        }
    }

    fn card_scope() -> EventScope {
        EventScope::Card {
            card: CardId::from("k"),
            wave: WaveId::from("w"),
            cove: CoveId::from("c"),
        }
    }
    fn wave_scope() -> EventScope {
        EventScope::Wave {
            wave: WaveId::from("w"),
            cove: CoveId::from("c"),
        }
    }
    fn cove_scope() -> EventScope {
        EventScope::Cove {
            cove: CoveId::from("c"),
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
        assert!(f.matches(&env(cove_scope(), card_added("c1", "w"))));
        assert!(f.matches(&env(wave_scope(), card_added("c1", "w"))));
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
        // Not a glob — `terminal.*` would not match here even if expressed
        // as the literal pattern; we only have exact match.
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
        assert!(!f.matches(&env(cove_scope(), codex_req())));
        assert!(!f.matches(&env(card_scope(), codex_req())));
    }

    #[test]
    fn scope_cove_exact_vs_descendants() {
        let exact = SubscribeFilter {
            scope: SubscribeScope::Cove(CoveId::from("c")),
            include_descendants: false,
            kinds: None,
        };
        assert!(exact.matches(&env(cove_scope(), codex_req())));
        // No descendants: a wave under this cove is out.
        assert!(!exact.matches(&env(wave_scope(), codex_req())));
        assert!(!exact.matches(&env(card_scope(), codex_req())));

        let desc = SubscribeFilter {
            scope: SubscribeScope::Cove(CoveId::from("c")),
            include_descendants: true,
            kinds: None,
        };
        assert!(desc.matches(&env(cove_scope(), codex_req())));
        assert!(desc.matches(&env(wave_scope(), codex_req())));
        assert!(desc.matches(&env(card_scope(), codex_req())));
        // Different cove out of scope.
        let other = EventScope::Wave {
            wave: WaveId::from("w2"),
            cove: CoveId::from("c2"),
        };
        assert!(!desc.matches(&env(other, codex_req())));
    }

    #[test]
    fn scope_wave_exact_vs_descendants() {
        let exact = SubscribeFilter {
            scope: SubscribeScope::Wave(WaveId::from("w")),
            include_descendants: false,
            kinds: None,
        };
        assert!(exact.matches(&env(wave_scope(), codex_req())));
        assert!(!exact.matches(&env(card_scope(), codex_req())));

        let desc = SubscribeFilter {
            scope: SubscribeScope::Wave(WaveId::from("w")),
            include_descendants: true,
            kinds: None,
        };
        assert!(desc.matches(&env(wave_scope(), codex_req())));
        assert!(desc.matches(&env(card_scope(), codex_req())));
        // Cove-only scope: no wave -> out.
        assert!(!desc.matches(&env(cove_scope(), codex_req())));
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
            wave: WaveId::from("w"),
            cove: CoveId::from("c"),
        };
        assert!(!f.matches(&env(other, codex_req())));
        assert!(!f.matches(&env(wave_scope(), codex_req())));
    }

    #[test]
    fn scope_anywave_with_and_without_descendants() {
        let no_desc = SubscribeFilter {
            scope: SubscribeScope::AnyWave,
            include_descendants: false,
            kinds: None,
        };
        assert!(no_desc.matches(&env(wave_scope(), codex_req())));
        assert!(!no_desc.matches(&env(card_scope(), codex_req())));
        assert!(!no_desc.matches(&env(cove_scope(), codex_req())));

        let desc = SubscribeFilter {
            scope: SubscribeScope::AnyWave,
            include_descendants: true,
            kinds: None,
        };
        assert!(desc.matches(&env(wave_scope(), codex_req())));
        assert!(desc.matches(&env(card_scope(), codex_req())));
        assert!(!desc.matches(&env(cove_scope(), codex_req())));
    }

    #[test]
    fn scope_anycard_matches_only_card() {
        let f = SubscribeFilter {
            scope: SubscribeScope::AnyCard,
            include_descendants: true,
            kinds: None,
        };
        assert!(f.matches(&env(card_scope(), codex_req())));
        assert!(!f.matches(&env(wave_scope(), codex_req())));
        assert!(!f.matches(&env(EventScope::System, codex_req())));
    }

    #[test]
    fn kind_then_scope_short_circuit() {
        // Kind mismatch short-circuits before scope is examined.
        let f = SubscribeFilter {
            scope: SubscribeScope::Cove(CoveId::from("c")),
            include_descendants: true,
            kinds: Some(vec!["codex.worker_requested".into()]),
        };
        // Right cove, but wrong kind.
        assert!(!f.matches(&env(cove_scope(), task_failed())));
        // Right kind + right cove (descendant card).
        assert!(f.matches(&env(card_scope(), codex_req())));
    }
}
