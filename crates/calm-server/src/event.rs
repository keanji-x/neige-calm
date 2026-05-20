//! Event bus + envelope shapes.
//!
//! Mutating handlers (REST `routes/*`, plugin overlay writes, terminal lifecycle)
//! `state.events.emit(...)` after a successful write. The WS `/events` handler
//! in `ws::events` subscribes to the bus and forwards filtered events to the UI.
//!
//! Wire format: `{"ev": "<dotted.name>", "data": {...}}`. The frontend's TS
//! `Event` type is auto-generated from this enum via `ts-rs` and lives at
//! `web/src/api/generated-events.ts`. The runtime zod validator in
//! `web/src/api/schemas.ts` is type-pinned to that emitted TS type via an
//! `expectTypeOf` conformance test, so any drift between this enum and the
//! frontend fails at the type-check step. See D7 / issue #5.

use crate::model::{Card, Cove, Overlay, Wave};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;
use ts_rs::TS;

/// Capacity of the broadcast channel. If a subscriber lags more than this,
/// it'll receive a `Lagged` error and the server drops its connection — the
/// client is expected to reconnect and re-fetch.
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

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    /// Send an event. Returns silently if there are no current subscribers.
    pub fn emit(&self, ev: Event) {
        let _ = self.tx.send(ev);
    }

    /// New subscriber. The receiver picks up events emitted after this call.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
