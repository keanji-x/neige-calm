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
//!      `EventBus` wrapped in a `BroadcastEnvelope { id, event }`.
//!
//! The wrapper guarantees the *commit-then-emit* invariant: if the txn
//! rolls back, neither the entity row nor the event row exists, and the
//! broadcast never fires. Conversely, a successful broadcast is always
//! backed by a persisted event row, which the eventual Scope D replay
//! protocol relies on.
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
//!
//! Wire format: `{"_id": 1729, "ev": "<dotted.name>", "data": {...}}`. The
//! frontend's TS `Event` type is auto-generated from this enum via `ts-rs`
//! and lives at `web/src/api/generated-events.ts`. The runtime zod
//! validator in `web/src/api/schemas.ts` is type-pinned to that emitted
//! TS type via an `expectTypeOf` conformance test, so any drift between
//! this enum and the frontend fails at the type-check step. See D7 /
//! issue #5.

use crate::model::{Card, Cove, Overlay, Wave};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;
use ts_rs::TS;

/// Capacity of the broadcast channel. If a subscriber lags more than this,
/// it'll receive a `Lagged` error and the server drops its connection — the
/// client is expected to reconnect and re-fetch (and once Scope D lands,
/// resume via `since=<lastId>`).
const BUS_CAPACITY: usize = 1024;

/// The full set of WS event envelopes the kernel emits on `/api/events`.
///
/// `ts-rs` derives a matching TypeScript discriminated union, written to
/// `web/src/api/generated-events.ts` when `cargo test export_bindings_` runs
/// (driven by `npm run gen:api`). The serde `tag`/`content` attributes are
/// honored — the emitted TS uses the same `{ ev, data }` envelope.
///
/// Note for future variants: ts-rs requires every payload type referenced
/// here to also derive `TS`. Inline struct variants (e.g. `CoveDeleted { id }`)
/// are emitted directly; tuple variants over a named struct (e.g.
/// `CoveUpdated(Cove)`) pull in the struct's own export.
#[derive(Clone, Debug, Serialize, TS)]
#[serde(tag = "ev", content = "data")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum Event {
    #[serde(rename = "cove.updated")]
    CoveUpdated(Cove),
    #[serde(rename = "cove.deleted")]
    CoveDeleted { id: String },

    #[serde(rename = "wave.updated")]
    WaveUpdated(Wave),
    #[serde(rename = "wave.deleted")]
    WaveDeleted { id: String, cove_id: String },

    #[serde(rename = "card.added")]
    CardAdded(Card),
    #[serde(rename = "card.updated")]
    CardUpdated(Card),
    #[serde(rename = "card.deleted")]
    CardDeleted { id: String, wave_id: String },

    #[serde(rename = "overlay.set")]
    OverlaySet(Overlay),
    #[serde(rename = "overlay.deleted")]
    OverlayDeleted {
        plugin_id: String,
        entity_kind: String,
        entity_id: String,
        kind: String,
    },

    #[serde(rename = "plugin.state")]
    PluginState {
        id: String,
        state: String,
        /// Crash reason / initialize-rejected message, surfaced to the WS so
        /// the UI can show it without a separate `/log` fetch. `None` for
        /// healthy transitions (Spawning → Running, etc.). Wire shape locked
        /// in design doc §7.
        ///
        /// `#[serde(default, skip_serializing_if = "Option::is_none")]`
        /// combined with `#[ts(optional)]` matches the runtime behavior: the
        /// field is absent on the wire when the inner `Option` is `None`, and
        /// the TS type marks it as `last_error?: string`. (Without `optional`,
        /// ts-rs would emit `last_error: string | null` which would diverge
        /// from what the server actually serializes.)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        last_error: Option<String>,
    },

    /// Codex CLI hook passthrough. The `neige-codex-bridge` subprocess POSTs
    /// each hook event payload to `/internal/codex/hook`; the route packages
    /// it into this variant and emits to the bus. The shape is intentionally
    /// opaque (Value) — codex's hook payload is documented but evolves, and
    /// the frontend codex card pattern-matches on `kind` (`hook.codex.<event>`)
    /// rather than typing every field.
    #[serde(rename = "codex.hook")]
    CodexHook {
        /// Owning card id — topic key `card:<card_id>`.
        card_id: String,
        /// Snake_case discriminator: `hook.codex.<event_name>` (e.g.
        /// `hook.codex.pre_tool_use`). Derived from `hook_event_name` in
        /// the codex payload; defaults to `hook.codex.unknown` if missing.
        kind: String,
        /// Original codex hook JSON, verbatim.
        #[ts(type = "unknown")]
        payload: Value,
    },
}

impl Event {
    /// String tag for the events-table `kind` column. Matches the
    /// `#[serde(rename = "...")]` on each variant. Centralized here so the
    /// `Repo::write_with_event` insert and the `events.kind` index agree
    /// on spelling without re-parsing the serialized envelope.
    pub fn kind_tag(&self) -> &'static str {
        match self {
            Event::CoveUpdated(_) => "cove.updated",
            Event::CoveDeleted { .. } => "cove.deleted",
            Event::WaveUpdated(_) => "wave.updated",
            Event::WaveDeleted { .. } => "wave.deleted",
            Event::CardAdded(_) => "card.added",
            Event::CardUpdated(_) => "card.updated",
            Event::CardDeleted { .. } => "card.deleted",
            Event::OverlaySet(_) => "overlay.set",
            Event::OverlayDeleted { .. } => "overlay.deleted",
            Event::PluginState { .. } => "plugin.state",
            Event::CodexHook { .. } => "codex.hook",
        }
    }

    /// Extract just the `data` payload (the inner content the
    /// `#[serde(tag, content)]` representation puts under `data`). Used by
    /// the events-table insert so we persist the bare payload, not the full
    /// `{ev, data}` envelope.
    pub fn payload_value(&self) -> serde_json::Value {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(mut map)) => {
                map.remove("data").unwrap_or(serde_json::Value::Null)
            }
            // Non-object serialization is impossible given the
            // `#[serde(tag, content)]` representation, but be conservative.
            _ => serde_json::Value::Null,
        }
    }
}

/// Subscription topics an `Event` matches. The WS handler intersects this with
/// each client's `sub` filter to decide forward-or-drop.
///
/// **Topic grammar** (mirror in frontend):
///   - `cove:<id>`           — events touching a specific cove
///   - `wave:<id>`           — events touching a specific wave
///   - `card:<id>`           — events touching a specific card
///   - `plugin:<id>`         — events emitted by/about a specific plugin
///   - `plugin:*`            — all plugin events
///   - `*`                   — firehose (debug only)
pub fn topics(ev: &Event) -> Vec<String> {
    match ev {
        Event::CoveUpdated(c) => vec![format!("cove:{}", c.id), "*".into()],
        Event::CoveDeleted { id } => vec![format!("cove:{}", id), "*".into()],

        Event::WaveUpdated(w) => vec![
            format!("wave:{}", w.id),
            format!("cove:{}", w.cove_id),
            "*".into(),
        ],
        Event::WaveDeleted { id, cove_id } => vec![
            format!("wave:{}", id),
            format!("cove:{}", cove_id),
            "*".into(),
        ],

        Event::CardAdded(c) | Event::CardUpdated(c) => vec![
            format!("card:{}", c.id),
            format!("wave:{}", c.wave_id),
            "*".into(),
        ],
        Event::CardDeleted { id, wave_id } => vec![
            format!("card:{}", id),
            format!("wave:{}", wave_id),
            "*".into(),
        ],

        Event::OverlaySet(o) => vec![
            format!("{}:{}", o.entity_kind, o.entity_id),
            format!("plugin:{}", o.plugin_id),
            "plugin:*".into(),
            "*".into(),
        ],
        Event::OverlayDeleted {
            plugin_id,
            entity_kind,
            entity_id,
            ..
        } => vec![
            format!("{}:{}", entity_kind, entity_id),
            format!("plugin:{}", plugin_id),
            "plugin:*".into(),
            "*".into(),
        ],

        Event::PluginState { id, .. } => {
            vec![format!("plugin:{}", id), "plugin:*".into(), "*".into()]
        }

        Event::CodexHook { card_id, .. } => {
            vec![format!("card:{}", card_id), "*".into()]
        }
    }
}

/// What the broadcast channel actually carries. The `id` is the row id
/// returned by `events.id`'s AUTOINCREMENT insert (see `Repo::write_with_event`
/// and `Repo::log_pure_event`). The WS handler uses it to stamp `_id` on the
/// outgoing JSON envelope per design doc §2.4.
///
/// We don't derive `Serialize` here — the serialization of the envelope into
/// the wire JSON is hand-rolled in `ws::events::handle` (it has to splice
/// `_id` alongside the existing `{ev, data}` flat shape rather than nest
/// `event` as a sub-object).
#[derive(Clone, Debug)]
pub struct BroadcastEnvelope {
    /// Assigned `events.id`. `0` is reserved (never produced by the
    /// auto-increment), used here as a sentinel for "no persisted row" in
    /// out-of-scope code paths that haven't yet been migrated to
    /// `write_with_event` / `log_pure_event`. Scope A converts every site
    /// the design doc names; any future emitter that bypasses the wrapper
    /// will surface as `_id: 0` on the wire, which is a useful canary.
    pub id: i64,
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
    pub(crate) fn emit_envelope(&self, env: BroadcastEnvelope) {
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
    /// Available outside `#[cfg(test)]` because integration tests in
    /// `crates/calm-server/tests/` consume the library through normal
    /// linkage — they don't see `#[cfg(test)]`-gated items.
    pub fn emit(&self, ev: Event) {
        let _ = self.tx.send(BroadcastEnvelope { id: 0, event: ev });
    }

    /// New subscriber. The receiver picks up envelopes emitted after this
    /// call.
    pub fn subscribe(&self) -> broadcast::Receiver<BroadcastEnvelope> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
