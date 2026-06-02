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

use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::model::{Card, Cove, Overlay, Wave, WaveLifecycle};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;
use ts_rs::TS;

// ---------------------------------------------------------------------------
// ArtifactRef — placeholder identifier for #129 Artifact Stream
// ---------------------------------------------------------------------------

/// Opaque identifier for a worker-produced artifact (file write, structured
/// output blob, etc.). PR4 of #136 introduces this as a **placeholder**:
/// the real Artifact Stream lands in #129, which will expand the type with
/// hash / content-type / storage-uri fields.
///
/// Today the variant is referenced only by `Event::TaskCompleted.artifacts`,
/// which carries a list of these so the dispatcher's push path can hand a
/// spec card a manifest of what its worker produced. Keep this minimal —
/// #129 territory expands the shape, not PR4.
///
/// Wire shape is a bare string via `#[serde(transparent)]`, matching the
/// typed-id pattern in [`crate::ids`]. ts-rs emits `export type ArtifactRef
/// = string;` so the frontend stays a thin alias.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct ArtifactRef(pub String);

impl ArtifactRef {
    /// Borrow the underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ArtifactRef {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ArtifactRef {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for ArtifactRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl AsRef<str> for ArtifactRef {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// EditAuthor — who produced a `WaveReportEdited` write
// ---------------------------------------------------------------------------

/// Producer of a single wave-report edit. Carried on every
/// `Event::WaveReportEdited` so PR4's UI can attribute timeline entries
/// without re-parsing the envelope's `actor` field, and so PR5's spec
/// system prompt can react to user-authored edits specifically.
///
/// PR2 only emits `EditAuthor::Spec` — the spec-MCP `calm.report.*`
/// tools are the only write path that exists today. PR3 introduces a
/// REST entry for human edits and starts emitting `EditAuthor::User`;
/// `EditAuthor::Kernel` is reserved for future server-internal
/// rewrites (FSM-driven scaffolding, migrations, etc.). Adding a
/// variant later is a non-breaking change for the wire shape (the
/// schema gains a new union arm, old clients see an unknown tag and
/// can ignore) but the persisted history rows must keep round-tripping,
/// so don't rename existing arms.
///
/// Wire shape matches the surrounding event-payload conventions
/// (`#[serde(rename_all = "lowercase")]`): `"spec"`, `"user"`,
/// `"kernel"` — the bare discriminator a JSON field gets when the enum
/// is referenced from an inline struct variant. No `tag`/`content`
/// dance: `EditAuthor` only ever appears as a payload field, never as
/// its own envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum EditAuthor {
    /// Spec card calling one of the `calm.report.{write,edit}` MCP
    /// tools. The only producer PR2 emits.
    Spec,
    /// Human-driven edit through a REST endpoint. Wired in PR3.
    User,
    /// Server-internal rewrite — FSM scaffolding, migrations, etc.
    /// Reserved; no emitter today.
    Kernel,
}

// ---------------------------------------------------------------------------
// EventScope — every event's "home scope"
// ---------------------------------------------------------------------------

/// Where an event lives in the cove → wave → card hierarchy.
///
/// PR2 of #136 stamps a scope on every persisted event so future PRs can
/// filter / route / authorize without re-parsing the event payload:
///
///   * PR3 (`enforce_role`) gates writes per card scope.
///   * PR5 (`SubscribeFilter` + `Dispatcher`) routes notifications + work
///     queues by wave scope, and the dispatcher's push path (#293) resolves
///     a wave's spec card to deliver task/report events as turn inputs.
///
/// `EventScope::System` is the catch-all for events that genuinely don't
/// belong to a single cove/wave/card (`Event::PluginState`, the
/// CoveCreated case where the cove doesn't exist before the event, and
/// the legacy NULL-on-replay fallback for pre-PR2 history rows). Pick
/// `System` only when you've ruled out the more specific scopes — a
/// `System`-tagged event opts out of every per-scope filter that follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", content = "id")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum EventScope {
    /// No entity scope — server-internal or cross-entity event.
    System,
    /// Scoped to one cove. No wave or card context.
    Cove { cove: CoveId },
    /// Scoped to one wave. Carries the owning cove for filter ergonomics
    /// (`scope_cove IS NOT NULL` already narrows the rowset for cove-level
    /// subscribers without a join).
    Wave { wave: WaveId, cove: CoveId },
    /// Scoped to one card. Carries wave + cove for the same reason.
    Card {
        card: CardId,
        wave: WaveId,
        cove: CoveId,
    },
}

impl EventScope {
    /// String discriminator stored in `events.scope_kind`. Stable: changing
    /// these strings would silently break the replay path. Mirrors the
    /// `#[serde(tag = "kind")]` variant names lowercased.
    pub fn kind(&self) -> &'static str {
        match self {
            EventScope::System => "system",
            EventScope::Cove { .. } => "cove",
            EventScope::Wave { .. } => "wave",
            EventScope::Card { .. } => "card",
        }
    }

    /// Owning cove id, if the scope carries one. PR5 will fan subscribers
    /// out per-cove from this without re-parsing the variant.
    pub fn cove_id(&self) -> Option<&CoveId> {
        match self {
            EventScope::System => None,
            EventScope::Cove { cove } => Some(cove),
            EventScope::Wave { cove, .. } => Some(cove),
            EventScope::Card { cove, .. } => Some(cove),
        }
    }

    /// Owning wave id, if the scope is wave-or-narrower.
    pub fn wave_id(&self) -> Option<&WaveId> {
        match self {
            EventScope::System | EventScope::Cove { .. } => None,
            EventScope::Wave { wave, .. } => Some(wave),
            EventScope::Card { wave, .. } => Some(wave),
        }
    }

    /// Card id, only for the card scope.
    pub fn card_id(&self) -> Option<&CardId> {
        match self {
            EventScope::Card { card, .. } => Some(card),
            _ => None,
        }
    }

    /// Reconstruct the scope from the four `events.scope_*` columns. Used
    /// by the replay path to recover the typed scope from a row.
    ///
    /// **NULL-tolerant**: a pre-PR2 row (NULL `scope_kind` is impossible
    /// thanks to the column default, but defensive nonetheless) or any
    /// row whose ancestor cols don't line up with the declared `kind`
    /// falls back to `EventScope::System`. The fallback is deliberate —
    /// the replay path must never strand a client because of a malformed
    /// scope.
    pub fn from_row(
        kind: Option<&str>,
        cove: Option<&str>,
        wave: Option<&str>,
        card: Option<&str>,
    ) -> EventScope {
        match kind.unwrap_or("system") {
            "cove" => match cove {
                Some(c) => EventScope::Cove {
                    cove: CoveId::from(c),
                },
                None => EventScope::System,
            },
            "wave" => match (wave, cove) {
                (Some(w), Some(c)) => EventScope::Wave {
                    wave: WaveId::from(w),
                    cove: CoveId::from(c),
                },
                _ => EventScope::System,
            },
            "card" => match (card, wave, cove) {
                (Some(card), Some(w), Some(c)) => EventScope::Card {
                    card: CardId::from(card),
                    wave: WaveId::from(w),
                    cove: CoveId::from(c),
                },
                _ => EventScope::System,
            },
            // "system" or anything unknown.
            _ => EventScope::System,
        }
    }
}

/// Capacity of the broadcast channel. If a subscriber lags more than this,
/// it'll receive a `Lagged` error and the server drops its connection — the
/// client is expected to reconnect and re-fetch (and once Scope D lands,
/// resume via `since=<lastId>`).
const BUS_CAPACITY: usize = 1024;

/// Sync-engine event envelope version. Stamped onto every
/// `BroadcastEnvelope` the kernel emits (both fresh writes and replay rows)
/// and persisted on each `events` row via the `event_version` column added
/// in migration `0006_events_version.sql`. Old rows that predate the
/// migration backfill to `1` automatically via the column default.
///
/// The matching migration default and this constant must move together —
/// when the envelope wire shape evolves in a way replicas need to gate on,
/// bump this and ship a new migration that defaults to the new value.
///
/// Surfaced on the wire under the camelCase key `eventVersion` (see
/// `ws::events::render_envelope`), and surfaced via `GET /api/version` as
/// `syncEventVersion` so the web client can refuse to replay a log it
/// doesn't understand. Sync event log is a Tier-A persistence contract per
/// `docs/upgrade-stability.md`.
pub const SYNC_EVENT_VERSION: u32 = 1;

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
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(tag = "ev", content = "data")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum Event {
    #[serde(rename = "cove.updated")]
    CoveUpdated(Cove),
    #[serde(rename = "cove.deleted")]
    CoveDeleted { id: CoveId },

    #[serde(rename = "wave.updated")]
    WaveUpdated(Wave),
    #[serde(rename = "wave.deleted")]
    WaveDeleted { id: WaveId, cove_id: CoveId },

    /// Issue #145 — explicit Wave lifecycle transition.
    ///
    /// Emitted exactly once per (validated) `from → to` change. Carries
    /// the wave id + the typed `from` / `to` so reducers downstream can
    /// drive UI updates without re-parsing every `WaveUpdated` payload.
    /// Wave-scoped: routes to `wave:<id>` and `cove:<cove>` subscribers.
    ///
    /// The state machine that gates which (from, to, actor) triples
    /// produce this envelope lives in `crate::wave_lifecycle`. Illegal
    /// transitions surface as `CalmError::Forbidden` at the call site
    /// and **no event is persisted**.
    ///
    /// Cleaner than overloading `WaveUpdated`: lifecycle subscribers
    /// (sidebar pills, Today schedule, the spec agent's own status
    /// loop) can filter on `kind = wave.lifecycle_changed` without
    /// inspecting every wave-row update for a possibly-unchanged
    /// `lifecycle` field.
    #[serde(rename = "wave.lifecycle_changed")]
    WaveLifecycleChanged {
        id: WaveId,
        cove_id: CoveId,
        from: WaveLifecycle,
        to: WaveLifecycle,
    },

    #[serde(rename = "card.added")]
    CardAdded(Card),
    #[serde(rename = "card.updated")]
    CardUpdated(Card),
    #[serde(rename = "card.deleted")]
    CardDeleted { id: CardId, wave_id: WaveId },

    /// Issue #247 PR2 — structured wave-report edit-log entry. Emitted
    /// alongside `Event::CardUpdated` from every
    /// `mcp_server::tools::wave_report::persist_report` call so PR4's UI
    /// can render an edit timeline and PR5's spec agent can wake on
    /// user-authored edits.
    ///
    /// `CardUpdated` stays the generic "the row changed, re-fetch" signal
    /// every existing frontend subscriber already consumes — `WaveReportEdited`
    /// is the *additional* structured edit-log entry, not a replacement.
    /// Both events land in the same transaction; the broadcast order
    /// matches the persisted order so a subscriber that wants edit-log
    /// semantics can ignore `CardUpdated` for wave-report cards without
    /// missing anything.
    ///
    /// `summary_before` / `body_before` are the projected text values
    /// **before** the `ReportDoc::update` call; `*_after` are the
    /// projected values **after**. Under single-writer SQLite they
    /// equal the caller's inputs verbatim; we read from the projection
    /// so the log entry stays bit-for-bit consistent with whatever the
    /// JSON cache and CRDT both end up persisting (matches the
    /// "projection is the truth" contract in `persist_report`).
    ///
    /// `author` is hard-coded to [`EditAuthor::Spec`] in PR2 — the
    /// spec-MCP tools are the only write path. PR3 plumbs an `Actor`
    /// through `persist_report` and starts emitting
    /// [`EditAuthor::User`] for REST-driven edits.
    ///
    /// `edit_id` is a fresh UUID v4 per call so PR4's UI can collapse
    /// adjacent retries or correlate timeline entries with the
    /// REST-side request id (PR3) without parsing the broader
    /// `BroadcastEnvelope.id`. It's persisted on the event payload —
    /// changing the format here would silently strand audit-log
    /// replay.
    ///
    /// Card-scoped: the kernel persists this row with
    /// `scope_wave = wave_id` and `scope_card = card_id` so the
    /// dispatcher's push filter can subscribe to a single wave's edit
    /// log without scanning the firehose.
    #[serde(rename = "wave.report_edited")]
    WaveReportEdited {
        wave_id: WaveId,
        card_id: CardId,
        author: EditAuthor,
        edit_id: String,
        summary_before: String,
        summary_after: String,
        body_before: String,
        body_after: String,
    },

    #[serde(rename = "overlay.set")]
    OverlaySet(Overlay),
    #[serde(rename = "overlay.deleted")]
    OverlayDeleted {
        plugin_id: String,
        entity_kind: String,
        entity_id: String,
        kind: String,
    },

    /// Terminal row removed (today: emitted by the orphan-terminal sweeper
    /// at `crate::terminal_sweeper`; a future user-initiated delete endpoint
    /// would emit the same variant). Carries the terminal id plus the
    /// card_id the row pointed at — useful for audit log lookups even
    /// though the card itself may have been deleted in an earlier event.
    /// Topic mapping (see `topics`): `terminal:<id>` plus the firehose.
    #[serde(rename = "terminal.deleted")]
    TerminalDeleted { id: String, card_id: CardId },

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
        card_id: CardId,
        /// Snake_case discriminator: `hook.codex.<event_name>` (e.g.
        /// `hook.codex.pre_tool_use`). Derived from `hook_event_name` in
        /// the codex payload; defaults to `hook.codex.unknown` if missing.
        kind: String,
        /// Original codex hook JSON, verbatim.
        #[ts(type = "unknown")]
        payload: Value,
    },

    /// Claude CLI hook passthrough. Same shape as [`Event::CodexHook`];
    /// PR-A only introduces the event identity and plumbing. Ingest and
    /// lifecycle interpretation land in later PRs.
    #[serde(rename = "claude.hook")]
    ClaudeHook {
        /// Owning card id — topic key `card:<card_id>`.
        card_id: CardId,
        /// Hook discriminator supplied by the future Claude hook route.
        kind: String,
        /// Original Claude hook JSON, verbatim.
        #[ts(type = "unknown")]
        payload: Value,
    },

    /// Spec/worker card asks the kernel dispatcher to spawn a codex worker
    /// card. PR4 of #136 introduced this **schema-only**. PR5's `Dispatcher`
    /// subscribes to `kinds=["*.requested"]` on the event bus and reacts by
    /// minting a worker card; the dispatcher's push path (#293) then delivers
    /// the matching `task.completed` / `task.failed` to the requesting spec
    /// card as a turn input.
    ///
    /// `idempotency_key` lets the dispatcher dedupe replays — a retried
    /// MCP call surfaces the same key and the dispatcher short-circuits to
    /// the existing worker card / pending result.
    ///
    /// `context` is opaque payload (working-dir hints, prior turn history,
    /// model preference). Kernel never inspects it; PR5's dispatcher
    /// forwards verbatim into the spawned worker's card payload.
    #[serde(rename = "codex.job_requested")]
    CodexJobRequested {
        idempotency_key: String,
        goal: String,
        #[ts(type = "unknown")]
        context: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        acceptance_criteria: Option<String>,
    },

    /// Spec card asks the kernel dispatcher to spawn a terminal worker
    /// card. PR4 schema-only; PR5's `Dispatcher` is the consumer.
    ///
    /// `cwd` is `None` when the spec card defers to the wave/cove default
    /// working directory.
    #[serde(rename = "terminal.job_requested")]
    TerminalJobRequested {
        idempotency_key: String,
        cmd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        cwd: Option<String>,
    },

    /// Worker card reports task completion. PR4 schema-only; the
    /// dispatcher's push path delivers this to the requesting spec card. The
    /// `idempotency_key` echoes back the one from the matching
    /// `*.job_requested` event so the spec can correlate without parsing
    /// the worker card's identity.
    ///
    /// `result` is opaque agent payload (free-form text, structured
    /// output, etc.); `artifacts` carries a list of [`ArtifactRef`]s the
    /// worker produced (file writes, blobs). PR4's `ArtifactRef` is a
    /// placeholder for #129's Artifact Stream — the full type lands there.
    #[serde(rename = "task.completed")]
    TaskCompleted {
        idempotency_key: String,
        #[ts(type = "unknown")]
        result: Value,
        artifacts: Vec<ArtifactRef>,
    },

    /// Worker card reports task failure. PR4 schema-only; the dispatcher's
    /// push path delivers this to the requesting spec card.
    ///
    /// `reason` is a free-form failure string — the kernel never parses
    /// it, but persists it on the events table so audit-log replay can
    /// surface the rationale a worker gave its spec.
    #[serde(rename = "task.failed")]
    TaskFailed {
        idempotency_key: String,
        reason: String,
    },

    /// #318 INV-1 (b) — boot takeover has given up on this wave's spec
    /// push channel. Emitted exactly once by
    /// [`crate::try_takeover_one_wave`] when the inert classifier fires
    /// (`thread/resume` → `-32600 "no rollout"`, spawn/connect failure,
    /// `mkdir` failure on the per-card sock dir, etc.) — i.e. every code
    /// path that returns [`crate::TakeoverOutcome::Inert`].
    ///
    /// This closes the R1-B1 "no-skip" observability gap from #315
    /// review: when takeover marks a wave inert, the spec card is
    /// excluded from `legacy spec takeover query` on subsequent boots,
    /// so any wave-scoped event already persisted in the events log
    /// (with `events.id > push_watermark`) would otherwise sit stranded
    /// forever — the events row stays, but nothing will ever deliver it
    /// to a spec thread. Without this signal, SRE / future
    /// crash-recovery code has no durable record of "this wave needs
    /// operator attention before its events can be replayed".
    ///
    /// `last_envelope_id` is the largest `events.id` of any envelope
    /// scoped to this wave that existed at the moment of inert
    /// classification. It's an upper bound on the abandoned set — every
    /// id in `(push_watermark, last_envelope_id]` for this wave is at
    /// risk. The kernel never *parses* this field; consumers (operator
    /// dashboards, the future re-run path from #313 problem #2) use it
    /// to size the stranded set. A wave with no in-scope events at the
    /// time of abandonment surfaces `last_envelope_id = 0`, the same
    /// sentinel `events.id` reserves for "no row".
    ///
    /// `cove_id` is on the payload for topic-routing symmetry with
    /// [`Event::WaveDeleted`]: an operator subscribing to a cove sees
    /// the abandonment without needing a separate join to recover the
    /// wave's owning cove.
    ///
    /// Wave-scoped on the persisted row (`scope_wave = wave_id`,
    /// `scope_cove = cove_id`). Topic mapping mirrors `WaveDeleted`
    /// (`wave:<id>`, `cove:<cove_id>`, firehose).
    ///
    /// **Forward-compat note**: this is a strictly-additive variant. Old
    /// clients see an unknown `ev` tag and ignore it; the WS replay
    /// path's `from_kind_and_payload` round-trips it like every other
    /// variant.
    #[serde(rename = "spec_push.abandoned")]
    SpecPushAbandoned {
        wave_id: WaveId,
        cove_id: CoveId,
        last_envelope_id: i64,
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
            Event::WaveLifecycleChanged { .. } => "wave.lifecycle_changed",
            Event::CardAdded(_) => "card.added",
            Event::CardUpdated(_) => "card.updated",
            Event::CardDeleted { .. } => "card.deleted",
            Event::WaveReportEdited { .. } => "wave.report_edited",
            Event::OverlaySet(_) => "overlay.set",
            Event::OverlayDeleted { .. } => "overlay.deleted",
            Event::TerminalDeleted { .. } => "terminal.deleted",
            Event::PluginState { .. } => "plugin.state",
            Event::CodexHook { .. } => "codex.hook",
            Event::ClaudeHook { .. } => "claude.hook",
            Event::CodexJobRequested { .. } => "codex.job_requested",
            Event::TerminalJobRequested { .. } => "terminal.job_requested",
            Event::TaskCompleted { .. } => "task.completed",
            Event::TaskFailed { .. } => "task.failed",
            Event::SpecPushAbandoned { .. } => "spec_push.abandoned",
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

    /// Rebuild a typed `Event` from the `(kind, payload)` pair stored in the
    /// `events` table. The wrapper splices the row's `kind` into the `ev`
    /// tag and `payload` JSON into the `data` content slot, then runs the
    /// derived `Deserialize` impl over the synthesized envelope.
    ///
    /// Used by Scope D's WS replay path: rows come back from the events
    /// table as `(id, kind, payload_text)`, and the WS handler reconstitutes
    /// each into a real `Event` so `topics(&ev)` can filter against the
    /// connection's subscription set the same way it does for live frames.
    pub fn from_kind_and_payload(
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<Self, serde_json::Error> {
        let envelope = serde_json::json!({ "ev": kind, "data": payload });
        serde_json::from_value(envelope)
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

        Event::WaveLifecycleChanged { id, cove_id, .. } => vec![
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

        // Issue #247 PR2 — wave-report edit log. Card-scoped on the
        // events row; topic mapping mirrors `Card*` so a subscriber
        // listening on the report card (or its wave) sees the
        // structured edit alongside the generic `card.updated`.
        Event::WaveReportEdited {
            wave_id, card_id, ..
        } => vec![
            format!("card:{}", card_id),
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

        Event::TerminalDeleted { id, .. } => vec![format!("terminal:{}", id), "*".into()],

        Event::PluginState { id, .. } => {
            vec![format!("plugin:{}", id), "plugin:*".into(), "*".into()]
        }

        Event::CodexHook { card_id, .. } | Event::ClaudeHook { card_id, .. } => {
            vec![format!("card:{}", card_id), "*".into()]
        }

        // PR4 of #136: kernel-internal dispatcher / task-lifecycle signals.
        // No card/wave/cove ids on the payload itself (the BroadcastEnvelope
        // carries the originating `EventScope` instead — see `Dispatcher`).
        // Subscribers identify these via the firehose plus the dispatcher's
        // `kinds=` filter (PR5).
        Event::CodexJobRequested { .. }
        | Event::TerminalJobRequested { .. }
        | Event::TaskCompleted { .. }
        | Event::TaskFailed { .. } => vec!["*".into()],

        // #318 INV-1 (b) — abandonment carries the wave/cove on the
        // payload (symmetry with `WaveDeleted`), so a cove- or wave-
        // scoped subscriber sees the stranding without needing the
        // BroadcastEnvelope's scope field.
        Event::SpecPushAbandoned {
            wave_id, cove_id, ..
        } => vec![
            format!("wave:{}", wave_id),
            format!("cove:{}", cove_id),
            "*".into(),
        ],
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
    /// `["codex.job_requested", "terminal.job_requested"]`.
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
    /// under it. The dispatcher uses `true` so a `*.job_requested`
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
    Cove(CoveId),
    /// Match an exact wave (or any card under it when
    /// `include_descendants = true`).
    Wave(WaveId),
    /// Match an exact card. `include_descendants` is meaningless here
    /// (cards have no children in the sync-engine hierarchy) but the
    /// filter still honors the flag uniformly — true or false, only
    /// the exact card matches.
    Card(CardId),
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

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn scope_kind_strings_pinned() {
        // These strings are persisted to `events.scope_kind` — changing
        // them is a wire break. Pin against accidental rename.
        assert_eq!(EventScope::System.kind(), "system");
        assert_eq!(
            EventScope::Cove {
                cove: CoveId::from("c")
            }
            .kind(),
            "cove"
        );
        assert_eq!(
            EventScope::Wave {
                wave: WaveId::from("w"),
                cove: CoveId::from("c"),
            }
            .kind(),
            "wave"
        );
        assert_eq!(
            EventScope::Card {
                card: CardId::from("k"),
                wave: WaveId::from("w"),
                cove: CoveId::from("c"),
            }
            .kind(),
            "card"
        );
    }

    #[test]
    fn ancestor_accessors_return_chain() {
        let s = EventScope::Card {
            card: CardId::from("k"),
            wave: WaveId::from("w"),
            cove: CoveId::from("c"),
        };
        assert_eq!(s.card_id().map(|x| x.as_str()), Some("k"));
        assert_eq!(s.wave_id().map(|x| x.as_str()), Some("w"));
        assert_eq!(s.cove_id().map(|x| x.as_str()), Some("c"));

        let s = EventScope::Wave {
            wave: WaveId::from("w"),
            cove: CoveId::from("c"),
        };
        assert_eq!(s.card_id(), None);
        assert_eq!(s.wave_id().map(|x| x.as_str()), Some("w"));
        assert_eq!(s.cove_id().map(|x| x.as_str()), Some("c"));

        let s = EventScope::Cove {
            cove: CoveId::from("c"),
        };
        assert!(s.card_id().is_none() && s.wave_id().is_none());
        assert_eq!(s.cove_id().map(|x| x.as_str()), Some("c"));

        let s = EventScope::System;
        assert!(s.card_id().is_none() && s.wave_id().is_none() && s.cove_id().is_none());
    }

    #[test]
    fn serde_round_trip_all_variants() {
        for s in [
            EventScope::System,
            EventScope::Cove {
                cove: CoveId::from("c"),
            },
            EventScope::Wave {
                wave: WaveId::from("w"),
                cove: CoveId::from("c"),
            },
            EventScope::Card {
                card: CardId::from("k"),
                wave: WaveId::from("w"),
                cove: CoveId::from("c"),
            },
        ] {
            let json = serde_json::to_string(&s).expect("serialize");
            let back: EventScope = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, s, "round-trip mismatch for {s:?} via {json}");
        }
    }

    #[test]
    fn serde_card_shape_pinned() {
        // Lock the on-wire shape so a future serde attribute change can't
        // silently break the WS envelope contract. `#[serde(tag = "kind",
        // content = "id")]` encodes a tuple variant as `{kind, id}` where
        // `id` carries the struct fields.
        let s = EventScope::Card {
            card: CardId::from("k"),
            wave: WaveId::from("w"),
            cove: CoveId::from("c"),
        };
        let v: serde_json::Value = serde_json::to_value(&s).unwrap();
        assert_eq!(v["kind"], "Card");
        assert_eq!(v["id"]["card"], "k");
        assert_eq!(v["id"]["wave"], "w");
        assert_eq!(v["id"]["cove"], "c");

        // `System` unit variant: just the `kind` discriminator, no `id`.
        let v: serde_json::Value = serde_json::to_value(EventScope::System).unwrap();
        assert_eq!(v["kind"], "System");
    }

    #[test]
    fn from_row_recovers_typed_scope() {
        assert_eq!(
            EventScope::from_row(Some("system"), None, None, None),
            EventScope::System,
        );
        assert_eq!(
            EventScope::from_row(Some("cove"), Some("c"), None, None),
            EventScope::Cove {
                cove: CoveId::from("c"),
            },
        );
        assert_eq!(
            EventScope::from_row(Some("wave"), Some("c"), Some("w"), None),
            EventScope::Wave {
                wave: WaveId::from("w"),
                cove: CoveId::from("c"),
            },
        );
        assert_eq!(
            EventScope::from_row(Some("card"), Some("c"), Some("w"), Some("k")),
            EventScope::Card {
                card: CardId::from("k"),
                wave: WaveId::from("w"),
                cove: CoveId::from("c"),
            },
        );
    }

    #[test]
    fn from_row_null_fallback_to_system() {
        // NULL kind → System.
        assert_eq!(
            EventScope::from_row(None, None, None, None),
            EventScope::System,
        );
        // Unknown kind → System.
        assert_eq!(
            EventScope::from_row(Some("plugin"), None, None, None),
            EventScope::System,
        );
        // Declared kind but missing required ancestor → System (replay
        // never strands a client on malformed scope).
        assert_eq!(
            EventScope::from_row(Some("card"), Some("c"), Some("w"), None),
            EventScope::System,
        );
        assert_eq!(
            EventScope::from_row(Some("wave"), Some("c"), None, None),
            EventScope::System,
        );
    }

    // ----- PR4 of #136: new Event variants + ArtifactRef -----------------
    //
    // These tests pin the wire shape of the dispatcher / task-lifecycle
    // variants and the `ArtifactRef` placeholder. The dispatcher (PR5) and
    // the web zod schemas both rely on a stable wire shape.

    #[test]
    fn artifact_ref_transparent_serde() {
        // `#[serde(transparent)]` keeps the wire shape a bare string —
        // never `{"0":"foo"}`. Mirrors the typed-id pattern in `ids.rs`.
        let r = ArtifactRef::from("artifact-1");
        assert_eq!(serde_json::to_string(&r).unwrap(), r#""artifact-1""#);
        let back: ArtifactRef = serde_json::from_str(r#""artifact-1""#).unwrap();
        assert_eq!(back, r);
        assert_eq!(r.as_str(), "artifact-1");
        assert_eq!(format!("{r}"), "artifact-1");
    }

    #[test]
    fn kind_tag_new_variants_pinned() {
        // The kind_tag strings are persisted to `events.kind` and surfaced
        // on the wire as the `ev` discriminator — changing them is a wire
        // break. Pin each PR4 variant explicitly.
        let codex_req = Event::CodexJobRequested {
            idempotency_key: "k".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
        };
        assert_eq!(codex_req.kind_tag(), "codex.job_requested");

        let term_req = Event::TerminalJobRequested {
            idempotency_key: "k".into(),
            cmd: "ls".into(),
            cwd: None,
        };
        assert_eq!(term_req.kind_tag(), "terminal.job_requested");

        let done = Event::TaskCompleted {
            idempotency_key: "k".into(),
            result: serde_json::Value::Null,
            artifacts: vec![],
        };
        assert_eq!(done.kind_tag(), "task.completed");

        let failed = Event::TaskFailed {
            idempotency_key: "k".into(),
            reason: "boom".into(),
        };
        assert_eq!(failed.kind_tag(), "task.failed");

        let claude_hook = Event::ClaudeHook {
            card_id: CardId::from("card-1"),
            kind: "hook.claude.stop".into(),
            payload: serde_json::Value::Null,
        };
        assert_eq!(claude_hook.kind_tag(), "claude.hook");
    }

    #[test]
    fn claude_hook_serde_round_trip_kind_and_topics() {
        let ev = Event::ClaudeHook {
            card_id: CardId::from("card-claude"),
            kind: "hook.claude.pre_tool_use".into(),
            payload: serde_json::json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
            }),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "claude.hook");
        assert_eq!(json["data"]["card_id"], "card-claude");
        assert_eq!(json["data"]["kind"], "hook.claude.pre_tool_use");
        assert_eq!(json["data"]["payload"]["tool_name"], "Bash");

        let back: Event = serde_json::from_value(json).unwrap();
        match back {
            Event::ClaudeHook {
                card_id,
                kind,
                payload,
            } => {
                assert_eq!(card_id.as_str(), "card-claude");
                assert_eq!(kind, "hook.claude.pre_tool_use");
                assert_eq!(payload["hook_event_name"], "PreToolUse");
            }
            other => panic!("expected ClaudeHook after round-trip, got {other:?}"),
        }

        let replay = Event::from_kind_and_payload(
            "claude.hook",
            serde_json::json!({
                "card_id": "card-claude",
                "kind": "hook.claude.stop",
                "payload": { "hook_event_name": "Stop" },
            }),
        )
        .expect("replay decode ClaudeHook");
        assert_eq!(replay.kind_tag(), "claude.hook");

        let t = topics(&replay);
        assert!(t.iter().any(|s| s == "card:card-claude"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
    }

    #[test]
    fn codex_job_requested_serde_round_trip() {
        let ev = Event::CodexJobRequested {
            idempotency_key: "idem-1".into(),
            goal: "refactor X".into(),
            context: serde_json::json!({ "cwd": "/tmp", "hints": [1, 2] }),
            acceptance_criteria: Some("tests pass".into()),
        };
        let json = serde_json::to_value(&ev).unwrap();
        // Pin the exact wire shape: `{ev, data}` envelope, snake_case keys.
        assert_eq!(json["ev"], "codex.job_requested");
        assert_eq!(json["data"]["idempotency_key"], "idem-1");
        assert_eq!(json["data"]["goal"], "refactor X");
        assert_eq!(json["data"]["context"]["cwd"], "/tmp");
        assert_eq!(json["data"]["acceptance_criteria"], "tests pass");

        // Round-trip via the Event enum.
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "codex.job_requested");

        // `acceptance_criteria = None` should be absent on the wire via
        // `skip_serializing_if`.
        let no_ac = Event::CodexJobRequested {
            idempotency_key: "k".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
        };
        let v = serde_json::to_value(&no_ac).unwrap();
        assert!(
            v["data"].get("acceptance_criteria").is_none(),
            "acceptance_criteria should be omitted when None, got {v}",
        );
    }

    #[test]
    fn terminal_job_requested_serde_round_trip() {
        let ev = Event::TerminalJobRequested {
            idempotency_key: "idem-2".into(),
            cmd: "cargo test".into(),
            cwd: Some("/repo".into()),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "terminal.job_requested");
        assert_eq!(json["data"]["idempotency_key"], "idem-2");
        assert_eq!(json["data"]["cmd"], "cargo test");
        assert_eq!(json["data"]["cwd"], "/repo");

        // `cwd = None` absent on the wire.
        let no_cwd = Event::TerminalJobRequested {
            idempotency_key: "k".into(),
            cmd: "ls".into(),
            cwd: None,
        };
        let v = serde_json::to_value(&no_cwd).unwrap();
        assert!(
            v["data"].get("cwd").is_none(),
            "cwd should be omitted when None, got {v}",
        );

        // Round-trip via the Event enum.
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "terminal.job_requested");
    }

    #[test]
    fn task_completed_serde_round_trip() {
        let ev = Event::TaskCompleted {
            idempotency_key: "idem-3".into(),
            result: serde_json::json!({ "summary": "ok", "lines": 42 }),
            artifacts: vec![ArtifactRef::from("a-1"), ArtifactRef::from("a-2")],
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "task.completed");
        assert_eq!(json["data"]["idempotency_key"], "idem-3");
        assert_eq!(json["data"]["result"]["summary"], "ok");
        // Artifacts are transparent strings on the wire — assert the array
        // shape so a future #129 expansion can't silently regress.
        assert_eq!(json["data"]["artifacts"][0], "a-1");
        assert_eq!(json["data"]["artifacts"][1], "a-2");

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "task.completed");
    }

    #[test]
    fn task_failed_serde_round_trip() {
        let ev = Event::TaskFailed {
            idempotency_key: "idem-4".into(),
            reason: "process exited with code 137".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "task.failed");
        assert_eq!(json["data"]["idempotency_key"], "idem-4");
        assert_eq!(json["data"]["reason"], "process exited with code 137");

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "task.failed");
    }

    // ----- PR2 of #247: EditAuthor + WaveReportEdited -------------------
    //
    // Pin the wire shape of the structured edit-log variant + its
    // sub-enum. PR4 (web UI) and PR5 (spec agent) both depend on this
    // shape; the persisted history rows depend on it forever.

    #[test]
    fn edit_author_lowercase_wire_shape() {
        // `#[serde(rename_all = "lowercase")]` — the on-wire
        // discriminator is the bare lowercase variant name. Pin each
        // arm so a future serde attribute change can't silently break
        // the persisted history row format.
        assert_eq!(
            serde_json::to_string(&EditAuthor::Spec).unwrap(),
            r#""spec""#
        );
        assert_eq!(
            serde_json::to_string(&EditAuthor::User).unwrap(),
            r#""user""#
        );
        assert_eq!(
            serde_json::to_string(&EditAuthor::Kernel).unwrap(),
            r#""kernel""#
        );

        // Round-trip back through Deserialize.
        for variant in [EditAuthor::Spec, EditAuthor::User, EditAuthor::Kernel] {
            let s = serde_json::to_string(&variant).unwrap();
            let back: EditAuthor = serde_json::from_str(&s).unwrap();
            assert_eq!(back, variant, "round-trip mismatch for {variant:?}");
        }
    }

    #[test]
    fn wave_report_edited_kind_tag_pinned() {
        // Persisted in `events.kind` + surfaced on the wire as the `ev`
        // discriminator. Changing the string is a wire break.
        let ev = wave_report_edited_sample();
        assert_eq!(ev.kind_tag(), "wave.report_edited");
    }

    #[test]
    fn wave_report_edited_serde_round_trip() {
        let ev = wave_report_edited_sample();
        let json = serde_json::to_value(&ev).unwrap();
        // Envelope shape: `{ ev, data }` per the enum's
        // `#[serde(tag = "ev", content = "data")]`.
        assert_eq!(json["ev"], "wave.report_edited");
        assert_eq!(json["data"]["wave_id"], "w-1");
        assert_eq!(json["data"]["card_id"], "card-1");
        assert_eq!(json["data"]["author"], "spec");
        assert_eq!(json["data"]["edit_id"], "edit-uuid-1");
        assert_eq!(json["data"]["summary_before"], "old summary");
        assert_eq!(json["data"]["summary_after"], "new summary");
        assert_eq!(json["data"]["body_before"], "old body");
        assert_eq!(json["data"]["body_after"], "new body");

        // Round-trip via the Event enum.
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "wave.report_edited");
        match back {
            Event::WaveReportEdited {
                wave_id,
                card_id,
                author,
                edit_id,
                summary_before,
                summary_after,
                body_before,
                body_after,
            } => {
                assert_eq!(wave_id.as_str(), "w-1");
                assert_eq!(card_id.as_str(), "card-1");
                assert_eq!(author, EditAuthor::Spec);
                assert_eq!(edit_id, "edit-uuid-1");
                assert_eq!(summary_before, "old summary");
                assert_eq!(summary_after, "new summary");
                assert_eq!(body_before, "old body");
                assert_eq!(body_after, "new body");
            }
            other => panic!("expected WaveReportEdited after round-trip, got {other:?}"),
        }
    }

    #[test]
    fn wave_report_edited_replay_via_from_kind_and_payload() {
        // Replay path — pin that `from_kind_and_payload` reconstitutes
        // the variant the same way the sync-engine replay does for
        // every other variant. Cover every `EditAuthor` arm so a
        // future serde tweak that breaks one of them surfaces here.
        for author_str in ["spec", "user", "kernel"] {
            let payload = serde_json::json!({
                "wave_id": "w-1",
                "card_id": "card-1",
                "author": author_str,
                "edit_id": "edit-uuid-1",
                "summary_before": "s0",
                "summary_after": "s1",
                "body_before": "b0",
                "body_after": "b1",
            });
            let ev = Event::from_kind_and_payload("wave.report_edited", payload)
                .unwrap_or_else(|e| panic!("replay decode failed for author={author_str}: {e}"));
            assert_eq!(ev.kind_tag(), "wave.report_edited");
            match ev {
                Event::WaveReportEdited { author, .. } => match (author_str, author) {
                    ("spec", EditAuthor::Spec)
                    | ("user", EditAuthor::User)
                    | ("kernel", EditAuthor::Kernel) => {}
                    (expected, actual) => {
                        panic!("author mismatch: expected {expected}, deserialized into {actual:?}")
                    }
                },
                other => panic!("expected WaveReportEdited, got {other:?}"),
            }
        }
    }

    #[test]
    fn wave_report_edited_topics_card_and_wave() {
        // Topic mapping — same shape as `Card*` so a subscriber
        // listening on the card or its wave sees the structured edit
        // alongside the generic `card.updated`.
        let ev = wave_report_edited_sample();
        let t = topics(&ev);
        assert!(t.iter().any(|s| s == "card:card-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "wave:w-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
    }

    fn wave_report_edited_sample() -> Event {
        Event::WaveReportEdited {
            wave_id: WaveId::from("w-1"),
            card_id: CardId::from("card-1"),
            author: EditAuthor::Spec,
            edit_id: "edit-uuid-1".into(),
            summary_before: "old summary".into(),
            summary_after: "new summary".into(),
            body_before: "old body".into(),
            body_after: "new body".into(),
        }
    }

    // ----- #318 INV-1 (b): Event::SpecPushAbandoned -----------------------
    //
    // Pin the wire shape of the abandonment signal emitted by
    // `try_takeover_one_wave` on the inert path. Persisted row format
    // and broadcast envelope shape are both wire contracts.

    #[test]
    fn spec_push_abandoned_kind_tag_pinned() {
        let ev = Event::SpecPushAbandoned {
            wave_id: WaveId::from("w-1"),
            cove_id: CoveId::from("c-1"),
            last_envelope_id: 42,
        };
        assert_eq!(ev.kind_tag(), "spec_push.abandoned");
    }

    #[test]
    fn spec_push_abandoned_serde_round_trip() {
        let ev = Event::SpecPushAbandoned {
            wave_id: WaveId::from("w-1"),
            cove_id: CoveId::from("c-1"),
            last_envelope_id: 42,
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "spec_push.abandoned");
        assert_eq!(json["data"]["wave_id"], "w-1");
        assert_eq!(json["data"]["cove_id"], "c-1");
        assert_eq!(json["data"]["last_envelope_id"], 42);

        let back: Event = serde_json::from_value(json).unwrap();
        match back {
            Event::SpecPushAbandoned {
                wave_id,
                cove_id,
                last_envelope_id,
            } => {
                assert_eq!(wave_id.as_str(), "w-1");
                assert_eq!(cove_id.as_str(), "c-1");
                assert_eq!(last_envelope_id, 42);
            }
            other => panic!("expected SpecPushAbandoned, got {other:?}"),
        }
    }

    #[test]
    fn spec_push_abandoned_replay_via_from_kind_and_payload() {
        let payload = serde_json::json!({
            "wave_id": "w-1",
            "cove_id": "c-1",
            "last_envelope_id": 7,
        });
        let ev = Event::from_kind_and_payload("spec_push.abandoned", payload)
            .expect("replay decode SpecPushAbandoned");
        assert_eq!(ev.kind_tag(), "spec_push.abandoned");
        match ev {
            Event::SpecPushAbandoned {
                wave_id,
                cove_id,
                last_envelope_id,
            } => {
                assert_eq!(wave_id.as_str(), "w-1");
                assert_eq!(cove_id.as_str(), "c-1");
                assert_eq!(last_envelope_id, 7);
            }
            other => panic!("expected SpecPushAbandoned, got {other:?}"),
        }
    }

    #[test]
    fn spec_push_abandoned_topics_wave_and_cove() {
        // Topic mapping — same shape as `WaveDeleted`: a subscriber
        // listening on the wave OR its cove sees the abandonment.
        let ev = Event::SpecPushAbandoned {
            wave_id: WaveId::from("w-1"),
            cove_id: CoveId::from("c-1"),
            last_envelope_id: 0,
        };
        let t = topics(&ev);
        assert!(t.iter().any(|s| s == "wave:w-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "cove:c-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
    }

    #[test]
    fn new_variants_round_trip_via_from_kind_and_payload() {
        // Replay path: `Event::from_kind_and_payload` reconstitutes a
        // typed Event from the `(kind, payload)` columns. Pin that the
        // PR4 variants survive this path so the eventual sync-engine
        // replay doesn't strand them.
        for (kind, payload) in [
            (
                "claude.hook",
                serde_json::json!({
                    "card_id": "card-1",
                    "kind": "hook.claude.stop",
                    "payload": {},
                }),
            ),
            (
                "codex.job_requested",
                serde_json::json!({
                    "idempotency_key": "k",
                    "goal": "g",
                    "context": {},
                }),
            ),
            (
                "terminal.job_requested",
                serde_json::json!({ "idempotency_key": "k", "cmd": "ls" }),
            ),
            (
                "task.completed",
                serde_json::json!({
                    "idempotency_key": "k",
                    "result": {},
                    "artifacts": [],
                }),
            ),
            (
                "task.failed",
                serde_json::json!({ "idempotency_key": "k", "reason": "r" }),
            ),
        ] {
            let ev = Event::from_kind_and_payload(kind, payload)
                .unwrap_or_else(|e| panic!("replay decode failed for {kind}: {e}"));
            assert_eq!(ev.kind_tag(), kind, "round-trip kind mismatch");
        }
    }
}

// ---------------------------------------------------------------------------
// SubscribeFilter unit tests (PR5 of #136)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod filter_tests {
    use super::*;

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
            deletable: true,
            created_at: 0,
            updated_at: 0,
        })
    }

    fn codex_req() -> Event {
        Event::CodexJobRequested {
            idempotency_key: "k".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
        }
    }

    fn task_failed() -> Event {
        Event::TaskFailed {
            idempotency_key: "k".into(),
            reason: "boom".into(),
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
                "codex.job_requested".into(),
                "terminal.job_requested".into(),
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
            kinds: Some(vec!["codex.job_requested".into()]),
        };
        // Right cove, but wrong kind.
        assert!(!f.matches(&env(cove_scope(), task_failed())));
        // Right kind + right cove (descendant card).
        assert!(f.matches(&env(card_scope(), codex_req())));
    }
}
