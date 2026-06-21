//! Event wire vocabulary — the data half of calm-server's `event` module
//! (#679 PR1).
//!
//! This module owns everything about events that is *shape*: the typed
//! [`Event`] enum (the ts-rs source for `web/src/api/generated-events.ts`),
//! its payload types ([`ArtifactRef`], [`WaveUpdatedPayload`],
//! [`EditAuthor`]), the persisted [`EventScope`], the
//! [`SYNC_EVENT_VERSION`] constant, the [`EventMetadata`] classifier and the
//! [`topics`] subscription mapping.
//!
//! The *transport* half — `EventBus` / `BroadcastEnvelope` /
//! `SubscribeFilter` (tokio broadcast) — stays in calm-server's `event`
//! module, which re-exports this one so `calm_server::event::*` paths are
//! unchanged. Split line per issue #679: "Event data types/serde/ts-rs →
//! calm-types; EventBus/BroadcastEnvelope (tokio broadcast) → calm-truth"
//! (calm-server until calm-truth exists).
//!
//! Wire format: `{"_id": 1729, "ev": "<dotted.name>", "data": {...}}`. The
//! frontend's TS `Event` type is auto-generated from this enum via `ts-rs`
//! and lives at `web/src/api/generated-events.ts`. The runtime zod
//! validator in `web/src/api/schemas.ts` is type-pinned to that emitted
//! TS type via an `expectTypeOf` conformance test, so any drift between
//! this enum and the frontend fails at the type-check step. See D7 /
//! issue #5.

use crate::harness::HarnessPhaseTag;
use crate::ids::{CardId, CoveId, WaveId};
use crate::model::{Card, Cove, Overlay, Wave, WaveLifecycle};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ops::Deref;
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

// ---------------------------------------------------------------------------
// WaveUpdatedPayload — wave row plus optional agent rationale
// ---------------------------------------------------------------------------

/// Payload for `Event::WaveUpdated`.
///
/// `wave` is flattened to preserve the historical wire shape: the event data
/// is still the full wave row at top level, with `agent_message` added as an
/// optional adjacent field for #597. Older persisted rows that lack the field
/// deserialize with `None`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WaveUpdatedPayload {
    #[serde(flatten)]
    pub wave: Wave,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_message: Option<String>,
}

impl WaveUpdatedPayload {
    pub fn new(wave: Wave, agent_message: Option<String>) -> Self {
        Self {
            wave,
            agent_message,
        }
    }
}

impl Deref for WaveUpdatedPayload {
    type Target = Wave;

    fn deref(&self) -> &Self::Target {
        &self.wave
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
///
/// Version history:
/// * `1` — initial envelope shape; added by migration 0006.
/// * `2` — dispatcher request event rename (issue #581). Wire kinds
///   `codex.job_requested` / `terminal.job_requested` are renamed to
///   `*.worker_requested`. An open v1-tab whose per-frame gate was
///   set to `syncEventVersion=1` at mount must drop new `eventVersion=2`
///   frames WITHOUT advancing the cursor; if we kept SYNC_EVENT_VERSION
///   at 1, those tabs would silently fail zod and advance past
///   invalidation frames. Old rows backfill to `1` via the migration
///   0006 column default.
/// * `3` — scheduler wire kinds (issue #644). Adds `plan.updated`
///   (PR-A) and `task.dispatched` (PR-B) to the event union. A v2 tab
///   whose per-frame gate cached `syncEventVersion=2` at mount would
///   treat `eventVersion=2` frames carrying the new kinds as in-range,
///   advance its replay cursor, then silently fail zod on the unknown
///   discriminator — permanently skipping the plan/dispatch
///   invalidation. Bumping to `3` makes those tabs drop the frames
///   WITHOUT advancing the cursor. Migration 0043 re-stamps any
///   `plan.updated` / `task.dispatched` rows persisted at version 2
///   before this bump shipped.
/// * `4` — gate-result wire kind (issue #644 PR-C). Adds
///   `task.gate_result` to the event union. A v3 tab whose per-frame
///   gate cached `syncEventVersion=3` at mount would otherwise advance
///   its replay cursor past the new variant and silently fail zod.
/// * `5` — workspace-lease wire kinds (issue #760 slice 1). Adds
///   `workspace.leased` and `workspace.released` to the event union.
///   A v4 tab would otherwise advance past those rows and fail zod on
///   replay before refreshing onto a bundle that understands them.
/// * `6` — plugin tool registration wire kind (#760 slice 2). Adds `plugin.tool.registered`.
/// * `7` — forge PR merge wire kind (issue #760 slice 6). Adds
///   `forge.pr.merged` to the event union. A v6 tab would otherwise
///   advance past those rows and fail zod before refreshing.
/// * `8` — git/forge toolset substrate kinds (issue #760 slice ③-a).
///   Adds 5 forge.* and 2 worktree.* events to the union.
/// * `9` — workflow registration descriptor events (issue #760 slice ④-a).
///   Adds `workflow.registered` to the event union.
pub const SYNC_EVENT_VERSION: u32 = 9;

/// Phase/slice PR identity carried by `forge.pr.merged`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct ForgeMergeSubject {
    pub phase: String,
    pub slice_id: String,
    pub pr_number: u64,
}

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
    WaveUpdated(WaveUpdatedPayload),
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    #[serde(rename = "card.added")]
    CardAdded(Card),
    #[serde(rename = "card.updated")]
    CardUpdated(Card),
    #[serde(rename = "card.deleted")]
    CardDeleted { id: CardId, wave_id: WaveId },

    #[serde(rename = "runtime.started")]
    RuntimeStarted {
        runtime_id: String,
        card_id: String,
        kind: crate::runtime::WorkerSessionKind,
        agent_provider: Option<crate::runtime::AgentProvider>,
        status: crate::worker::WorkerSessionState,
    },
    #[serde(rename = "runtime.status_changed")]
    RuntimeStatusChanged {
        runtime_id: String,
        card_id: String,
        old_status: crate::worker::WorkerSessionState,
        new_status: crate::worker::WorkerSessionState,
    },
    #[serde(rename = "runtime.superseded")]
    RuntimeSuperseded {
        old_runtime_id: String,
        new_runtime_id: String,
        card_id: String,
    },
    #[serde(rename = "harness.item.added")]
    HarnessItemAdded {
        runtime_id: String,
        card_id: CardId,
        wave_id: WaveId,
        item_db_id: i64,
        item_uuid: Option<String>,
        item_type: Option<String>,
        turn_id: Option<String>,
        method: String,
    },
    #[serde(rename = "harness.phase.changed")]
    HarnessPhaseChanged {
        runtime_id: String,
        card_id: CardId,
        wave_id: WaveId,
        old_phase: HarnessPhaseTag,
        new_phase: HarnessPhaseTag,
    },
    #[serde(rename = "harness.transcript.cleared")]
    HarnessTranscriptCleared {
        runtime_id: String,
        card_id: CardId,
        wave_id: WaveId,
    },
    /// #615 F1 — emitted when `POST /api/cards/{id}/spec/input` queues
    /// a user-authored text observation onto the spec harness. Card-scoped
    /// (the spec card), `wave_id` carried so wave-timeline subscribers can
    /// filter without a card→wave lookup. Actor is on the envelope
    /// (`X-Calm-Actor` → `events.actor`), `char_count` lets audit/replay
    /// surface size without keeping the body text on the event row.
    ///
    /// **Body text is intentionally not on the payload** — large free-form
    /// user input would balloon the events log and the body is already
    /// observable via the queued `Observation::UserMessage` snapshot +
    /// the subsequent turn input. We log size only.
    #[serde(rename = "harness.user_message.enqueued")]
    HarnessUserMessageEnqueued {
        runtime_id: String,
        card_id: CardId,
        wave_id: WaveId,
        char_count: u32,
    },

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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
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
    #[serde(rename = "plugin.tool.registered")]
    PluginToolRegistered {
        plugin_id: String,
        tool_name: String,
    },
    #[serde(rename = "workflow.registered", rename_all = "camelCase")]
    WorkflowRegistered {
        plugin_id: String,
        workflow_id: String,
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
        /// Stable hook ingest key used by the server and spec harness to
        /// suppress duplicate lifecycle posts.
        #[serde(default)]
        hook_idempotency_key: String,
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
        /// Stable hook ingest key used by the server and spec harness to
        /// suppress duplicate lifecycle posts.
        #[serde(default)]
        hook_idempotency_key: String,
        /// Original Claude hook JSON, verbatim.
        #[ts(type = "unknown")]
        payload: Value,
    },

    /// Deprecated: retired in #644 PR-D; retained for old-log
    /// deserialization only.
    ///
    /// Spec/worker card asked the kernel dispatcher to spawn a codex worker
    /// card. PR4 of #136 introduced this **schema-only**. PR5's `Dispatcher`
    /// subscribed to the event bus and reacted by minting a worker card;
    /// #644 PR-D removed that live dispatch arm. The wire shape remains so
    /// persisted logs replay.
    ///
    /// `idempotency_key` lets the dispatcher dedupe replays — a retried
    /// MCP call surfaces the same key and the dispatcher short-circuits to
    /// the existing worker card / pending result.
    ///
    /// `context` is opaque payload (working-dir hints, prior turn history,
    /// model preference). Kernel never inspects it; PR5's dispatcher
    /// forwards verbatim into the spawned worker's card payload.
    #[serde(rename = "codex.worker_requested", alias = "codex.job_requested")]
    CodexWorkerRequested {
        idempotency_key: String,
        goal: String,
        #[ts(type = "unknown")]
        context: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        acceptance_criteria: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    /// Deprecated: retired in #644 PR-D; retained for old-log
    /// deserialization only.
    ///
    /// Spec card asked the kernel dispatcher to spawn a terminal worker
    /// card. PR4 schema-only; PR5's `Dispatcher` was the consumer until
    /// #644 PR-D removed that live dispatch arm.
    ///
    /// `cwd` is `None` when the spec card defers to the wave/cove default
    /// working directory.
    #[serde(rename = "terminal.worker_requested", alias = "terminal.job_requested")]
    TerminalWorkerRequested {
        idempotency_key: String,
        cmd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    /// Worker card reports task completion. PR4 schema-only; the
    /// dispatcher's push path delivers this to the requesting spec card. The
    /// `idempotency_key` echoes back the one from the matching
    /// `*.worker_requested` event so the spec can correlate without parsing
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    /// Issue #644 — the spec revised the wave's task plan via
    /// `calm.plan.upsert` / `calm.plan.cancel`. Appended in the same
    /// eventized tx as the `tasks` row writes, wave-scoped, actor
    /// `AiSpec`. `changed_keys` lists the task keys whose rows were
    /// created/updated/canceled by the call (`unchanged` upserts are
    /// not listed). The PR-B scheduler subscribes to this kind as its
    /// primary trigger; until then it is an audit/UI record only.
    ///
    /// Spec-only: the in-tx role gate refuses this event from any AI
    /// worker actor, mirroring the dispatch-request rule (#583).
    #[serde(rename = "plan.updated")]
    PlanUpdated {
        wave_id: WaveId,
        changed_keys: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    /// Issue #644 PR-B — the kernel scheduler claimed a plan task
    /// (`pending → dispatched`). Appended **inside the claim tx** (design
    /// §5.4/§5.6) so the runs projection stays purely event-sourced: a
    /// scheduler-dispatched task has no `*.worker_requested` event, and
    /// this record is the projection's requested-record fallback
    /// (`requested_at`, `kind`, the `requested`/`running` statuses).
    ///
    /// `idempotency_key` is the task id (`"{wave_id}:{key}"`); `kind` is
    /// the worker kind (`"codex"` / `"terminal"`). Wave-scoped, actor
    /// `ActorId::KernelDispatcher`, kernel-only: the in-tx role gate
    /// refuses it from any card-derived actor (spec included) — only the
    /// scheduler may claim tasks.
    #[serde(rename = "task.dispatched")]
    TaskDispatched {
        idempotency_key: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },

    /// Issue #760 slice 1 — the kernel acquired a workflow-agnostic
    /// isolated workspace lease for a Codex task. The lease is just a
    /// directory plus a durable row; git/worktree semantics are layered
    /// by later plugin slices. The event is persisted with card scope,
    /// and the card/wave ids are also carried here so topic filtering can
    /// route replay/live frames without inspecting the envelope scope.
    #[serde(rename = "workspace.leased")]
    WorkspaceLeased {
        wave_id: WaveId,
        card_id: CardId,
        lease_id: String,
        path: String,
    },

    /// Issue #760 slice 1 — the kernel released a workspace lease after
    /// worker completion, compensation, or boot reclaim. The payload
    /// mirrors [`Event::WorkspaceLeased`] routing fields and carries the
    /// durable lease id for audit correlation.
    #[serde(rename = "workspace.released")]
    WorkspaceReleased {
        wave_id: WaveId,
        card_id: CardId,
        lease_id: String,
    },

    /// Issue #760 slice 6 — a forge adapter merged the authoritative
    /// phase/slice PR for a wave. `wave_id` and `subject` identify the
    /// C6/R4-4 target; `head_sha` and `merge_sha` are the forge output
    /// values extracted from the action result.
    #[serde(rename = "forge.pr.merged")]
    ForgePrMerged {
        wave_id: WaveId,
        subject: ForgeMergeSubject,
        head_sha: String,
        merge_sha: String,
    },
    #[serde(rename = "forge.scan.completed")]
    ForgeScanCompleted {
        wave_id: WaveId,
        overlapping_prs: Vec<u64>,
    },
    #[serde(rename = "forge.pr.opened")]
    ForgePrOpened {
        wave_id: WaveId,
        pr_number: u64,
        head_sha: String,
    },
    #[serde(rename = "forge.pr.diff.read")]
    ForgePrDiffRead {
        wave_id: WaveId,
        pr_number: u64,
        base_sha: String,
        head_sha: String,
        artifact_path: String,
    },
    #[serde(rename = "forge.pr.checks")]
    ForgePrChecks {
        wave_id: WaveId,
        pr_number: u64,
        conclusion: String,
    },
    #[serde(rename = "forge.issue.closed")]
    ForgeIssueClosed { wave_id: WaveId, issue_number: u64 },
    #[serde(rename = "worktree.provisioned")]
    WorktreeProvisioned {
        wave_id: WaveId,
        card_id: CardId,
        path: String,
    },
    #[serde(rename = "worktree.removed")]
    WorktreeRemoved {
        wave_id: WaveId,
        card_id: CardId,
        path: String,
    },

    /// Issue #644 PR-C (§6.5) — the kernel `task-verify` runner completed a
    /// `task-verify` attempt and recorded its verdict. Appended in the
    /// SAME tx as the `verifying → done|failed` tasks-row flip (the
    /// gate observer's completion tx, or the scheduler's reconcile
    /// backstop), wave-scoped, actor `ActorId::KernelDispatcher` —
    /// every kernel-emitted task event uses `KernelDispatcher` so
    /// `is_spec_verdict_event` never classifies it as a spec verdict
    /// (design §6.5).
    ///
    /// `task_id` and `idempotency_key` both carry the task id
    /// (`"{wave_id}:{key}"`); the duplicate key field keeps the
    /// task-event correlation convention every other `task.*` kind
    /// uses. `passed` is the machine verdict (wrapper exit 0);
    /// `failing_step` is the last `::gate-step` sentinel before a red
    /// exit; `log_tail` is the trailing ≤8KiB of the gate log;
    /// `attempt` is the gate attempt number `N` from the operation's
    /// `#g{N}` idempotency suffix. Kernel-only: the in-tx role gate
    /// refuses it from any card-derived actor or plugin.
    #[serde(rename = "task.gate_result")]
    TaskGateResult {
        task_id: String,
        idempotency_key: String,
        passed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        failing_step: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        exit_code: Option<i32>,
        log_tail: String,
        log_path: String,
        attempt: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        agent_message: Option<String>,
    },
}

/// Bounded typed result-extraction contract (R4-3). NOT a predicate DSL:
/// no booleans, no expressions, no array logic — only a target event kind
/// + named field reads (exit-code | JSON-pointer over the action's --json).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeEventSpec {
    pub event_kind: String,
    pub fields: std::collections::BTreeMap<String, FieldSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldSource {
    ExitCode,
    JsonField { path: String },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ForgeExtractError {
    #[error("forge event spec requires JSON stdout but none was provided")]
    MissingJsonStdout,
    #[error("forge event spec field `{field}` pointer `{path}` did not resolve")]
    PointerUnresolved { field: String, path: String },
}

impl ForgeEventSpec {
    /// Build the event `data` payload map from the action's exit code and
    /// optional --json stdout.
    ///
    /// STRICT-FAIL: a JsonField whose pointer does not resolve, or
    /// json_stdout=None while any JsonField is declared, is an Err. Values
    /// are taken AS-IS from the pointer (no coercion); the final
    /// typed-deserialize happens later via Event::from_kind_and_payload, so
    /// a type mismatch surfaces there. ExitCode -> JSON number.
    pub fn extract_payload(
        &self,
        exit_code: i32,
        json_stdout: Option<&serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, ForgeExtractError> {
        let needs_json = self
            .fields
            .values()
            .any(|source| matches!(source, FieldSource::JsonField { .. }));
        if needs_json && json_stdout.is_none() {
            return Err(ForgeExtractError::MissingJsonStdout);
        }

        let mut payload = serde_json::Map::new();
        for (field, source) in &self.fields {
            match source {
                FieldSource::ExitCode => {
                    payload.insert(field.clone(), serde_json::json!(exit_code));
                }
                FieldSource::JsonField { path } => {
                    let value =
                        json_stdout
                            .and_then(|json| json.pointer(path))
                            .ok_or_else(|| ForgeExtractError::PointerUnresolved {
                                field: field.clone(),
                                path: path.clone(),
                            })?;
                    payload.insert(field.clone(), value.clone());
                }
            }
        }
        Ok(payload)
    }
}

/// Central event-classifier result for the kernel's event surfaces.
///
/// This is the single place that combines the dotted event name
/// (`kind_tag`) with the three plugin-subscription classifier decisions
/// (`plugin_id`, `entity_kind`, `entity_id`). Keep the producing match in
/// [`Event::metadata`] exhaustive: PR5 of #136 hazard H1 deliberately avoids
/// `_ =>` catch-alls so adding an event variant forces an explicit classifier
/// decision instead of silently inheriting `None`.
///
/// `plugin_id` is set only for `Event::OverlaySet`,
/// `Event::OverlayDeleted`, `Event::PluginState`, and
/// `Event::PluginToolRegistered`, and `Event::WorkflowRegistered`; every other variant has no plugin
/// attribution. `entity_kind` / `entity_id` are set only for events with a
/// filterable entity surface. The PR4 dispatcher/task-lifecycle variants
/// (`Event::CodexWorkerRequested`, `Event::TerminalWorkerRequested`,
/// `Event::TaskCompleted`, `Event::TaskFailed`) carry no plugin id, entity
/// kind, or entity id; plugins that want those signals must filter via the
/// events glob clause and omit the classifier clauses.
///
/// Issue #247 PR2 treats `Event::WaveReportEdited` as card-scoped for plugin
/// filters: `entity_kind = "card"` and `entity_id = card_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventMetadata {
    pub kind_tag: &'static str,
    pub plugin_id: Option<String>,
    pub entity_kind: Option<String>,
    pub entity_id: Option<String>,
}

impl Event {
    /// Centralized event classifier surface used by persistence and plugin
    /// subscription filters. Keep this exhaustive so adding a variant forces a
    /// deliberate kind/plugin/entity decision in one place.
    ///
    /// `kind_tag` is captured from [`Event::kind_tag`] so the string literals
    /// live in one zero-allocation hot-path match. Calling `kind_tag()` per
    /// broadcast must not construct this metadata or clone classifier strings.
    pub fn metadata(&self) -> EventMetadata {
        let kind_tag = self.kind_tag();
        match self {
            Event::CoveUpdated(c) => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: Some(c.id.to_string()),
            },
            Event::CoveDeleted { id } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: Some(id.to_string()),
            },
            Event::WaveUpdated(w) => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("wave".into()),
                entity_id: Some(w.id.to_string()),
            },
            Event::WaveDeleted { id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("wave".into()),
                entity_id: Some(id.to_string()),
            },
            Event::WaveLifecycleChanged { id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("wave".into()),
                entity_id: Some(id.to_string()),
            },
            Event::CardAdded(c) => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(c.id.to_string()),
            },
            Event::CardUpdated(c) => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(c.id.to_string()),
            },
            Event::CardDeleted { id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(id.to_string()),
            },
            Event::RuntimeStarted { card_id, .. }
            | Event::RuntimeStatusChanged { card_id, .. }
            | Event::RuntimeSuperseded { card_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(card_id.to_string()),
            },
            Event::HarnessItemAdded { card_id, .. }
            | Event::HarnessPhaseChanged { card_id, .. }
            | Event::HarnessTranscriptCleared { card_id, .. }
            | Event::HarnessUserMessageEnqueued { card_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(card_id.to_string()),
            },
            Event::WaveReportEdited { card_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(card_id.to_string()),
            },
            Event::OverlaySet(o) => EventMetadata {
                kind_tag,
                plugin_id: Some(o.plugin_id.clone()),
                entity_kind: Some(o.entity_kind.clone()),
                entity_id: Some(o.entity_id.clone()),
            },
            Event::OverlayDeleted {
                plugin_id,
                entity_kind,
                entity_id,
                ..
            } => EventMetadata {
                kind_tag,
                plugin_id: Some(plugin_id.clone()),
                entity_kind: Some(entity_kind.clone()),
                entity_id: Some(entity_id.clone()),
            },
            Event::TerminalDeleted { id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: Some(id.clone()),
            },
            Event::PluginState { id, .. } => EventMetadata {
                kind_tag,
                plugin_id: Some(id.clone()),
                entity_kind: None,
                entity_id: Some(id.clone()),
            },
            Event::PluginToolRegistered { plugin_id, .. }
            | Event::WorkflowRegistered { plugin_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: Some(plugin_id.clone()),
                entity_kind: None,
                entity_id: Some(plugin_id.clone()),
            },
            Event::CodexHook { card_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(card_id.to_string()),
            },
            Event::ClaudeHook { card_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("card".into()),
                entity_id: Some(card_id.to_string()),
            },
            Event::CodexWorkerRequested { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
            Event::TerminalWorkerRequested { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
            Event::TaskCompleted { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
            Event::TaskFailed { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
            Event::PlanUpdated { wave_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("wave".into()),
                entity_id: Some(wave_id.to_string()),
            },
            // Issue #644 PR-B — like the other task-lifecycle signals:
            // no plugin / entity classification; consumers filter via the
            // events kind clause + the envelope's wave scope.
            Event::TaskDispatched { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
            Event::WorkspaceLeased { card_id, .. } | Event::WorkspaceReleased { card_id, .. } => {
                EventMetadata {
                    kind_tag,
                    plugin_id: None,
                    entity_kind: Some("card".into()),
                    entity_id: Some(card_id.to_string()),
                }
            }
            Event::ForgePrMerged { wave_id, .. }
            | Event::ForgeScanCompleted { wave_id, .. }
            | Event::ForgePrOpened { wave_id, .. }
            | Event::ForgePrDiffRead { wave_id, .. }
            | Event::ForgePrChecks { wave_id, .. }
            | Event::ForgeIssueClosed { wave_id, .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: Some("wave".into()),
                entity_id: Some(wave_id.to_string()),
            },
            Event::WorktreeProvisioned { card_id, .. } | Event::WorktreeRemoved { card_id, .. } => {
                EventMetadata {
                    kind_tag,
                    plugin_id: None,
                    entity_kind: Some("card".into()),
                    entity_id: Some(card_id.to_string()),
                }
            }
            // Issue #644 PR-C — like the other task-lifecycle signals:
            // no plugin / entity classification; consumers filter via
            // the events kind clause + the envelope's wave scope.
            Event::TaskGateResult { .. } => EventMetadata {
                kind_tag,
                plugin_id: None,
                entity_kind: None,
                entity_id: None,
            },
        }
    }

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
            Event::RuntimeStarted { .. } => "runtime.started",
            Event::RuntimeStatusChanged { .. } => "runtime.status_changed",
            Event::RuntimeSuperseded { .. } => "runtime.superseded",
            Event::HarnessItemAdded { .. } => "harness.item.added",
            Event::HarnessPhaseChanged { .. } => "harness.phase.changed",
            Event::HarnessTranscriptCleared { .. } => "harness.transcript.cleared",
            Event::HarnessUserMessageEnqueued { .. } => "harness.user_message.enqueued",
            Event::WaveReportEdited { .. } => "wave.report_edited",
            Event::OverlaySet(_) => "overlay.set",
            Event::OverlayDeleted { .. } => "overlay.deleted",
            Event::TerminalDeleted { .. } => "terminal.deleted",
            Event::PluginState { .. } => "plugin.state",
            Event::PluginToolRegistered { .. } => "plugin.tool.registered",
            Event::WorkflowRegistered { .. } => "workflow.registered",
            Event::CodexHook { .. } => "codex.hook",
            Event::ClaudeHook { .. } => "claude.hook",
            Event::CodexWorkerRequested { .. } => "codex.worker_requested",
            Event::TerminalWorkerRequested { .. } => "terminal.worker_requested",
            Event::TaskCompleted { .. } => "task.completed",
            Event::TaskFailed { .. } => "task.failed",
            Event::PlanUpdated { .. } => "plan.updated",
            Event::TaskDispatched { .. } => "task.dispatched",
            Event::WorkspaceLeased { .. } => "workspace.leased",
            Event::WorkspaceReleased { .. } => "workspace.released",
            Event::ForgePrMerged { .. } => "forge.pr.merged",
            Event::ForgeScanCompleted { .. } => "forge.scan.completed",
            Event::ForgePrOpened { .. } => "forge.pr.opened",
            Event::ForgePrDiffRead { .. } => "forge.pr.diff.read",
            Event::ForgePrChecks { .. } => "forge.pr.checks",
            Event::ForgeIssueClosed { .. } => "forge.issue.closed",
            Event::WorktreeProvisioned { .. } => "worktree.provisioned",
            Event::WorktreeRemoved { .. } => "worktree.removed",
            Event::TaskGateResult { .. } => "task.gate_result",
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
        // Runtime payloads intentionally route by card only: the §F payloads do
        // not carry wave_id, and today's web client subscribes with ["*"].
        // Future wave-only subscribers can revisit by threading EventScope into
        // topics() or adding wave_id to these payloads.
        Event::RuntimeStarted { card_id, .. }
        | Event::RuntimeStatusChanged { card_id, .. }
        | Event::RuntimeSuperseded { card_id, .. } => {
            vec![format!("card:{}", card_id), "*".into()]
        }
        Event::HarnessItemAdded {
            wave_id, card_id, ..
        }
        | Event::HarnessPhaseChanged {
            wave_id, card_id, ..
        }
        | Event::HarnessTranscriptCleared {
            wave_id, card_id, ..
        }
        | Event::HarnessUserMessageEnqueued {
            wave_id, card_id, ..
        } => vec![
            format!("card:{}", card_id),
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
        Event::PluginToolRegistered { plugin_id, .. }
        | Event::WorkflowRegistered { plugin_id, .. } => {
            vec![
                format!("plugin:{}", plugin_id),
                "plugin:*".into(),
                "*".into(),
            ]
        }

        Event::CodexHook { card_id, .. } | Event::ClaudeHook { card_id, .. } => {
            vec![format!("card:{}", card_id), "*".into()]
        }

        // PR4 of #136: kernel-internal dispatcher / task-lifecycle signals.
        // No card/wave/cove ids on the payload itself (the BroadcastEnvelope
        // carries the originating `EventScope` instead — see `Dispatcher`).
        // Subscribers identify these via the firehose plus the dispatcher's
        // `kinds=` filter (PR5).
        Event::CodexWorkerRequested { .. }
        | Event::TerminalWorkerRequested { .. }
        | Event::TaskCompleted { .. }
        | Event::TaskFailed { .. }
        | Event::TaskDispatched { .. }
        | Event::TaskGateResult { .. } => vec!["*".into()],

        Event::WorkspaceLeased {
            wave_id, card_id, ..
        }
        | Event::WorkspaceReleased {
            wave_id, card_id, ..
        } => vec![
            format!("card:{}", card_id),
            format!("wave:{}", wave_id),
            "*".into(),
        ],

        Event::ForgePrMerged { wave_id, .. }
        | Event::ForgeScanCompleted { wave_id, .. }
        | Event::ForgePrOpened { wave_id, .. }
        | Event::ForgePrDiffRead { wave_id, .. }
        | Event::ForgePrChecks { wave_id, .. }
        | Event::ForgeIssueClosed { wave_id, .. } => {
            vec![format!("wave:{}", wave_id), "*".into()]
        }

        Event::WorktreeProvisioned {
            wave_id, card_id, ..
        }
        | Event::WorktreeRemoved {
            wave_id, card_id, ..
        } => vec![
            format!("card:{}", card_id),
            format!("wave:{}", wave_id),
            "*".into(),
        ],

        // Issue #644 — plan revisions are wave-scoped on the payload, so
        // wave subscribers (future UI task list) can filter without the
        // firehose. No cove id on the payload; the BroadcastEnvelope's
        // EventScope carries the full ancestor chain.
        Event::PlanUpdated { wave_id, .. } => vec![format!("wave:{}", wave_id), "*".into()],
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
        let codex_req = Event::CodexWorkerRequested {
            idempotency_key: "k".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
            agent_message: None,
        };
        assert_eq!(codex_req.kind_tag(), "codex.worker_requested");

        let term_req = Event::TerminalWorkerRequested {
            idempotency_key: "k".into(),
            cmd: "ls".into(),
            cwd: None,
            agent_message: None,
        };
        assert_eq!(term_req.kind_tag(), "terminal.worker_requested");

        let done = Event::TaskCompleted {
            idempotency_key: "k".into(),
            result: serde_json::Value::Null,
            artifacts: vec![],
            agent_message: None,
        };
        assert_eq!(done.kind_tag(), "task.completed");

        let failed = Event::TaskFailed {
            idempotency_key: "k".into(),
            reason: "boom".into(),
            agent_message: None,
        };
        assert_eq!(failed.kind_tag(), "task.failed");

        let plan_updated = Event::PlanUpdated {
            wave_id: WaveId::from("wave-1"),
            changed_keys: vec!["impl-parser".into()],
            agent_message: None,
        };
        assert_eq!(plan_updated.kind_tag(), "plan.updated");

        let task_dispatched = Event::TaskDispatched {
            idempotency_key: "wave-1:impl-parser".into(),
            kind: "codex".into(),
            agent_message: None,
        };
        assert_eq!(task_dispatched.kind_tag(), "task.dispatched");

        let workspace_leased = Event::WorkspaceLeased {
            wave_id: WaveId::from("wave-1"),
            card_id: CardId::from("card-1"),
            lease_id: "lease-1".into(),
            path: ".claude/worktrees/wave-1/card-1".into(),
        };
        assert_eq!(workspace_leased.kind_tag(), "workspace.leased");

        let workspace_released = Event::WorkspaceReleased {
            wave_id: WaveId::from("wave-1"),
            card_id: CardId::from("card-1"),
            lease_id: "lease-1".into(),
        };
        assert_eq!(workspace_released.kind_tag(), "workspace.released");

        let forge_pr_merged = Event::ForgePrMerged {
            wave_id: WaveId::from("wave-1"),
            subject: ForgeMergeSubject {
                phase: "impl".into(),
                slice_id: "6".into(),
                pr_number: 760,
            },
            head_sha: "head-sha".into(),
            merge_sha: "merge-sha".into(),
        };
        assert_eq!(forge_pr_merged.kind_tag(), "forge.pr.merged");

        let forge_scan_completed = Event::ForgeScanCompleted {
            wave_id: WaveId::from("wave-1"),
            overlapping_prs: vec![1, 2],
        };
        assert_eq!(forge_scan_completed.kind_tag(), "forge.scan.completed");

        let forge_pr_opened = Event::ForgePrOpened {
            wave_id: WaveId::from("wave-1"),
            pr_number: 1,
            head_sha: "head-sha".into(),
        };
        assert_eq!(forge_pr_opened.kind_tag(), "forge.pr.opened");

        let forge_pr_diff_read = Event::ForgePrDiffRead {
            wave_id: WaveId::from("wave-1"),
            pr_number: 1,
            base_sha: "base-sha".into(),
            head_sha: "head-sha".into(),
            artifact_path: "/tmp/neige/forge-diff.patch".into(),
        };
        assert_eq!(forge_pr_diff_read.kind_tag(), "forge.pr.diff.read");

        let forge_pr_checks = Event::ForgePrChecks {
            wave_id: WaveId::from("wave-1"),
            pr_number: 1,
            conclusion: "success".into(),
        };
        assert_eq!(forge_pr_checks.kind_tag(), "forge.pr.checks");

        let forge_issue_closed = Event::ForgeIssueClosed {
            wave_id: WaveId::from("wave-1"),
            issue_number: 1,
        };
        assert_eq!(forge_issue_closed.kind_tag(), "forge.issue.closed");

        let worktree_provisioned = Event::WorktreeProvisioned {
            wave_id: WaveId::from("wave-1"),
            card_id: CardId::from("card-1"),
            path: "/tmp/worktree".into(),
        };
        assert_eq!(worktree_provisioned.kind_tag(), "worktree.provisioned");

        let worktree_removed = Event::WorktreeRemoved {
            wave_id: WaveId::from("wave-1"),
            card_id: CardId::from("card-1"),
            path: "/tmp/worktree".into(),
        };
        assert_eq!(worktree_removed.kind_tag(), "worktree.removed");

        let claude_hook = Event::ClaudeHook {
            card_id: CardId::from("card-1"),
            kind: "hook.claude.stop".into(),
            hook_idempotency_key: "hook-claude".into(),
            payload: serde_json::Value::Null,
        };
        assert_eq!(claude_hook.kind_tag(), "claude.hook");

        let runtime_started = Event::RuntimeStarted {
            runtime_id: "runtime-1".into(),
            card_id: "card-1".into(),
            kind: crate::runtime::WorkerSessionKind::CodexCard,
            agent_provider: Some(crate::runtime::AgentProvider::Codex),
            status: crate::worker::WorkerSessionState::Starting,
        };
        assert_eq!(runtime_started.kind_tag(), "runtime.started");

        let runtime_status_changed = Event::RuntimeStatusChanged {
            runtime_id: "runtime-1".into(),
            card_id: "card-1".into(),
            old_status: crate::worker::WorkerSessionState::Starting,
            new_status: crate::worker::WorkerSessionState::Running,
        };
        assert_eq!(runtime_status_changed.kind_tag(), "runtime.status_changed");

        let runtime_superseded = Event::RuntimeSuperseded {
            old_runtime_id: "runtime-1".into(),
            new_runtime_id: "runtime-2".into(),
            card_id: "card-1".into(),
        };
        assert_eq!(runtime_superseded.kind_tag(), "runtime.superseded");

        let transcript_cleared = Event::HarnessTranscriptCleared {
            runtime_id: "runtime-1".into(),
            card_id: CardId::from("card-1"),
            wave_id: WaveId::from("wave-1"),
        };
        assert_eq!(transcript_cleared.kind_tag(), "harness.transcript.cleared");

        let user_message_enqueued = Event::HarnessUserMessageEnqueued {
            runtime_id: "runtime-1".into(),
            card_id: CardId::from("card-1"),
            wave_id: WaveId::from("wave-1"),
            char_count: 5,
        };
        assert_eq!(
            user_message_enqueued.kind_tag(),
            "harness.user_message.enqueued"
        );

        let workflow_registered = Event::WorkflowRegistered {
            plugin_id: "dev.neige.git-forge".into(),
            workflow_id: "issue-development".into(),
        };
        assert_eq!(workflow_registered.kind_tag(), "workflow.registered");
    }

    #[test]
    fn event_metadata_covers_all_variants_via_kind_tag() {
        for ev in metadata_coverage_events() {
            let metadata = ev.metadata();
            assert_eq!(
                metadata.kind_tag,
                ev.kind_tag(),
                "metadata kind_tag mismatch for {ev:?}",
            );
        }
    }

    #[test]
    fn kind_tag_does_not_allocate_for_string_payload_variants() {
        // Called per broadcast; pin the API to a static string so variants
        // with String payloads do not need metadata construction or cloning.
        let ev = Event::OverlaySet(overlay_sample("p1", "card", "c1", "status"));
        let s: &'static str = ev.kind_tag();
        assert_eq!(s, "overlay.set");
    }

    #[test]
    fn event_metadata_overlay_carries_plugin_id_and_entity() {
        let ev = Event::OverlaySet(overlay_sample("p1", "card", "c1", "status"));
        let metadata = ev.metadata();

        assert_eq!(metadata.plugin_id.as_deref(), Some("p1"));
        assert_eq!(metadata.entity_kind.as_deref(), Some("card"));
        assert_eq!(metadata.entity_id.as_deref(), Some("c1"));
    }

    #[test]
    fn runtime_started_serde_round_trip() {
        let ev = Event::RuntimeStarted {
            runtime_id: "runtime-1".into(),
            card_id: "card-1".into(),
            kind: crate::runtime::WorkerSessionKind::CodexCard,
            agent_provider: Some(crate::runtime::AgentProvider::Codex),
            status: crate::worker::WorkerSessionState::Starting,
        };

        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "runtime.started");
        assert_eq!(json["data"]["runtime_id"], "runtime-1");
        assert_eq!(json["data"]["card_id"], "card-1");
        assert_eq!(json["data"]["kind"], "codex");
        assert_eq!(json["data"]["agent_provider"], "codex");
        assert_eq!(json["data"]["status"], "starting");

        let back: Event = serde_json::from_value(json).unwrap();
        match back {
            Event::RuntimeStarted {
                runtime_id,
                card_id,
                kind,
                agent_provider,
                status,
            } => {
                assert_eq!(runtime_id, "runtime-1");
                assert_eq!(card_id, "card-1");
                assert_eq!(kind, crate::runtime::WorkerSessionKind::CodexCard);
                assert_eq!(agent_provider, Some(crate::runtime::AgentProvider::Codex));
                assert_eq!(status, crate::worker::WorkerSessionState::Starting);
            }
            other => panic!("expected RuntimeStarted after round-trip, got {other:?}"),
        }
    }

    #[test]
    fn runtime_status_changed_serde_round_trip() {
        let ev = Event::RuntimeStatusChanged {
            runtime_id: "runtime-1".into(),
            card_id: "card-1".into(),
            old_status: crate::worker::WorkerSessionState::Starting,
            new_status: crate::worker::WorkerSessionState::Running,
        };

        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "runtime.status_changed");
        assert_eq!(json["data"]["runtime_id"], "runtime-1");
        assert_eq!(json["data"]["card_id"], "card-1");
        assert_eq!(json["data"]["old_status"], "starting");
        assert_eq!(json["data"]["new_status"], "running");

        let back: Event = serde_json::from_value(json).unwrap();
        match back {
            Event::RuntimeStatusChanged {
                runtime_id,
                card_id,
                old_status,
                new_status,
            } => {
                assert_eq!(runtime_id, "runtime-1");
                assert_eq!(card_id, "card-1");
                assert_eq!(old_status, crate::worker::WorkerSessionState::Starting);
                assert_eq!(new_status, crate::worker::WorkerSessionState::Running);
            }
            other => panic!("expected RuntimeStatusChanged after round-trip, got {other:?}"),
        }
    }

    #[test]
    fn runtime_superseded_serde_round_trip() {
        let ev = Event::RuntimeSuperseded {
            old_runtime_id: "runtime-1".into(),
            new_runtime_id: "runtime-2".into(),
            card_id: "card-1".into(),
        };

        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "runtime.superseded");
        assert_eq!(json["data"]["old_runtime_id"], "runtime-1");
        assert_eq!(json["data"]["new_runtime_id"], "runtime-2");
        assert_eq!(json["data"]["card_id"], "card-1");

        let back: Event = serde_json::from_value(json).unwrap();
        match back {
            Event::RuntimeSuperseded {
                old_runtime_id,
                new_runtime_id,
                card_id,
            } => {
                assert_eq!(old_runtime_id, "runtime-1");
                assert_eq!(new_runtime_id, "runtime-2");
                assert_eq!(card_id, "card-1");
            }
            other => panic!("expected RuntimeSuperseded after round-trip, got {other:?}"),
        }
    }

    #[test]
    fn claude_hook_serde_round_trip_kind_and_topics() {
        let ev = Event::ClaudeHook {
            card_id: CardId::from("card-claude"),
            kind: "hook.claude.pre_tool_use".into(),
            hook_idempotency_key: "hook-claude".into(),
            payload: serde_json::json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
            }),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "claude.hook");
        assert_eq!(json["data"]["card_id"], "card-claude");
        assert_eq!(json["data"]["kind"], "hook.claude.pre_tool_use");
        assert_eq!(json["data"]["hook_idempotency_key"], "hook-claude");
        assert_eq!(json["data"]["payload"]["tool_name"], "Bash");

        let back: Event = serde_json::from_value(json).unwrap();
        match back {
            Event::ClaudeHook {
                card_id,
                kind,
                hook_idempotency_key,
                payload,
            } => {
                assert_eq!(card_id.as_str(), "card-claude");
                assert_eq!(kind, "hook.claude.pre_tool_use");
                assert_eq!(hook_idempotency_key, "hook-claude");
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
        match &replay {
            Event::ClaudeHook {
                hook_idempotency_key,
                ..
            } => assert!(hook_idempotency_key.is_empty()),
            other => panic!("expected ClaudeHook replay, got {other:?}"),
        }

        let t = topics(&replay);
        assert!(t.iter().any(|s| s == "card:card-claude"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
    }

    #[test]
    fn codex_worker_requested_serde_round_trip() {
        let ev = Event::CodexWorkerRequested {
            idempotency_key: "idem-1".into(),
            goal: "refactor X".into(),
            context: serde_json::json!({ "cwd": "/tmp", "hints": [1, 2] }),
            acceptance_criteria: Some("tests pass".into()),
            agent_message: Some("dispatch rationale".into()),
        };
        let json = serde_json::to_value(&ev).unwrap();
        // Pin the exact wire shape: `{ev, data}` envelope, snake_case keys.
        assert_eq!(json["ev"], "codex.worker_requested");
        assert_eq!(json["data"]["idempotency_key"], "idem-1");
        assert_eq!(json["data"]["goal"], "refactor X");
        assert_eq!(json["data"]["context"]["cwd"], "/tmp");
        assert_eq!(json["data"]["acceptance_criteria"], "tests pass");
        assert_eq!(json["data"]["agent_message"], "dispatch rationale");

        // Round-trip via the Event enum.
        let back: Event = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.kind_tag(), "codex.worker_requested");

        // Backward compatibility for pre-#581 event-log rows / clients.
        let mut old_json = json;
        old_json["ev"] = serde_json::json!("codex.job_requested");
        let back: Event = serde_json::from_value(old_json).unwrap();
        assert_eq!(back.kind_tag(), "codex.worker_requested");

        // `acceptance_criteria = None` should be absent on the wire via
        // `skip_serializing_if`.
        let no_ac = Event::CodexWorkerRequested {
            idempotency_key: "k".into(),
            goal: "g".into(),
            context: serde_json::Value::Null,
            acceptance_criteria: None,
            agent_message: None,
        };
        let v = serde_json::to_value(&no_ac).unwrap();
        assert!(
            v["data"].get("acceptance_criteria").is_none(),
            "acceptance_criteria should be omitted when None, got {v}",
        );
    }

    #[test]
    fn terminal_worker_requested_serde_round_trip() {
        let ev = Event::TerminalWorkerRequested {
            idempotency_key: "idem-2".into(),
            cmd: "cargo test".into(),
            cwd: Some("/repo".into()),
            agent_message: Some("terminal rationale".into()),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "terminal.worker_requested");
        assert_eq!(json["data"]["idempotency_key"], "idem-2");
        assert_eq!(json["data"]["cmd"], "cargo test");
        assert_eq!(json["data"]["cwd"], "/repo");
        assert_eq!(json["data"]["agent_message"], "terminal rationale");

        // `cwd = None` absent on the wire.
        let no_cwd = Event::TerminalWorkerRequested {
            idempotency_key: "k".into(),
            cmd: "ls".into(),
            cwd: None,
            agent_message: None,
        };
        let v = serde_json::to_value(&no_cwd).unwrap();
        assert!(
            v["data"].get("cwd").is_none(),
            "cwd should be omitted when None, got {v}",
        );

        // Round-trip via the Event enum.
        let back: Event = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.kind_tag(), "terminal.worker_requested");

        // Backward compatibility for pre-#581 event-log rows / clients.
        let mut old_json = json;
        old_json["ev"] = serde_json::json!("terminal.job_requested");
        let back: Event = serde_json::from_value(old_json).unwrap();
        assert_eq!(back.kind_tag(), "terminal.worker_requested");
    }

    #[test]
    fn task_completed_serde_round_trip() {
        let ev = Event::TaskCompleted {
            idempotency_key: "idem-3".into(),
            result: serde_json::json!({ "summary": "ok", "lines": 42 }),
            artifacts: vec![ArtifactRef::from("a-1"), ArtifactRef::from("a-2")],
            agent_message: Some("accepted rationale".into()),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "task.completed");
        assert_eq!(json["data"]["idempotency_key"], "idem-3");
        assert_eq!(json["data"]["result"]["summary"], "ok");
        assert_eq!(json["data"]["agent_message"], "accepted rationale");
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
            agent_message: Some("rejected rationale".into()),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "task.failed");
        assert_eq!(json["data"]["idempotency_key"], "idem-4");
        assert_eq!(json["data"]["reason"], "process exited with code 137");
        assert_eq!(json["data"]["agent_message"], "rejected rationale");

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "task.failed");
    }

    #[test]
    fn task_gate_result_serde_round_trip() {
        // Issue #644 PR-C — pin the gate-result wire shape: zod's
        // `taskGateResultSchema` and the SYNC_EVENT_VERSION=4 history
        // bullet both depend on these exact field names.
        let ev = Event::TaskGateResult {
            task_id: "w-1:impl".into(),
            idempotency_key: "w-1:impl".into(),
            passed: false,
            failing_step: Some("clippy".into()),
            exit_code: Some(101),
            log_tail: "error: ...".into(),
            log_path: "/data/gate-logs/w-1:impl-g2.log".into(),
            attempt: 2,
            agent_message: None,
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["ev"], "task.gate_result");
        assert_eq!(json["data"]["task_id"], "w-1:impl");
        assert_eq!(json["data"]["idempotency_key"], "w-1:impl");
        assert_eq!(json["data"]["passed"], false);
        assert_eq!(json["data"]["failing_step"], "clippy");
        assert_eq!(json["data"]["exit_code"], 101);
        assert_eq!(json["data"]["log_tail"], "error: ...");
        assert_eq!(json["data"]["attempt"], 2);

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "task.gate_result");

        // Green verdict: the Option fields stay off the wire.
        let green = Event::TaskGateResult {
            task_id: "w-1:impl".into(),
            idempotency_key: "w-1:impl".into(),
            passed: true,
            failing_step: None,
            exit_code: Some(0),
            log_tail: String::new(),
            log_path: "/data/gate-logs/w-1:impl-g1.log".into(),
            attempt: 1,
            agent_message: None,
        };
        let json = serde_json::to_value(&green).unwrap();
        assert!(json["data"].get("failing_step").is_none());
        assert!(json["data"].get("agent_message").is_none());
    }

    #[test]
    fn workspace_lease_events_serde_round_trip_and_topics() {
        let leased = Event::WorkspaceLeased {
            wave_id: WaveId::from("wave-1"),
            card_id: CardId::from("card-1"),
            lease_id: "lease-1".into(),
            path: ".claude/worktrees/wave-1/card-1".into(),
        };
        let json = serde_json::to_value(&leased).unwrap();
        assert_eq!(json["ev"], "workspace.leased");
        assert_eq!(json["data"]["wave_id"], "wave-1");
        assert_eq!(json["data"]["card_id"], "card-1");
        assert_eq!(json["data"]["lease_id"], "lease-1");
        assert_eq!(json["data"]["path"], ".claude/worktrees/wave-1/card-1");

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "workspace.leased");
        assert_eq!(
            topics(&back),
            vec!["card:card-1", "wave:wave-1", "*"],
            "workspace lease events route by card and wave"
        );
        let meta = back.metadata();
        assert_eq!(meta.entity_kind.as_deref(), Some("card"));
        assert_eq!(meta.entity_id.as_deref(), Some("card-1"));

        let released = Event::WorkspaceReleased {
            wave_id: WaveId::from("wave-1"),
            card_id: CardId::from("card-1"),
            lease_id: "lease-1".into(),
        };
        let json = serde_json::to_value(&released).unwrap();
        assert_eq!(json["ev"], "workspace.released");
        assert_eq!(json["data"]["lease_id"], "lease-1");
        assert!(json["data"].get("path").is_none());

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "workspace.released");
        assert_eq!(
            topics(&back),
            vec!["card:card-1", "wave:wave-1", "*"],
            "workspace release events route by card and wave"
        );
    }

    #[test]
    fn forge_pr_merged_serde_round_trip_metadata_and_topics() {
        let merged = Event::ForgePrMerged {
            wave_id: WaveId::from("wave-1"),
            subject: ForgeMergeSubject {
                phase: "impl".into(),
                slice_id: "6".into(),
                pr_number: 760,
            },
            head_sha: "head-sha".into(),
            merge_sha: "merge-sha".into(),
        };
        let json = serde_json::to_value(&merged).unwrap();
        assert_eq!(json["ev"], "forge.pr.merged");
        assert_eq!(json["data"]["wave_id"], "wave-1");
        assert_eq!(json["data"]["subject"]["phase"], "impl");
        assert_eq!(json["data"]["subject"]["slice_id"], "6");
        assert_eq!(json["data"]["subject"]["pr_number"], 760);
        assert_eq!(json["data"]["head_sha"], "head-sha");
        assert_eq!(json["data"]["merge_sha"], "merge-sha");

        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back.kind_tag(), "forge.pr.merged");
        assert_eq!(
            topics(&back),
            vec!["wave:wave-1", "*"],
            "forge PR merge events route by wave"
        );
        let meta = back.metadata();
        assert_eq!(meta.plugin_id, None);
        assert_eq!(meta.entity_kind.as_deref(), Some("wave"));
        assert_eq!(meta.entity_id.as_deref(), Some("wave-1"));
    }

    #[test]
    fn forge_event_spec_extracts_json_fields_with_nested_array_pointer() {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "head_sha".into(),
            FieldSource::JsonField {
                path: "/oid".into(),
            },
        );
        fields.insert(
            "merge_sha".into(),
            FieldSource::JsonField {
                path: "/commits/0/oid".into(),
            },
        );
        let spec = ForgeEventSpec {
            event_kind: "forge.pr.merged".into(),
            fields,
        };
        let stdout = serde_json::json!({
            "oid": "head-sha",
            "commits": [{ "oid": "merge-sha" }],
        });

        let payload = spec.extract_payload(0, Some(&stdout)).unwrap();
        assert_eq!(
            payload.get("head_sha"),
            Some(&serde_json::json!("head-sha"))
        );
        assert_eq!(
            payload.get("merge_sha"),
            Some(&serde_json::json!("merge-sha"))
        );
    }

    #[test]
    fn forge_event_spec_missing_pointer_is_strict_error() {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "merge_sha".into(),
            FieldSource::JsonField {
                path: "/missing".into(),
            },
        );
        let spec = ForgeEventSpec {
            event_kind: "forge.pr.merged".into(),
            fields,
        };

        let err = spec
            .extract_payload(0, Some(&serde_json::json!({})))
            .unwrap_err();
        assert_eq!(
            err,
            ForgeExtractError::PointerUnresolved {
                field: "merge_sha".into(),
                path: "/missing".into(),
            }
        );
    }

    #[test]
    fn forge_event_spec_missing_json_stdout_is_strict_error() {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "head_sha".into(),
            FieldSource::JsonField {
                path: "/oid".into(),
            },
        );
        let spec = ForgeEventSpec {
            event_kind: "forge.pr.merged".into(),
            fields,
        };

        let err = spec.extract_payload(0, None).unwrap_err();
        assert_eq!(err, ForgeExtractError::MissingJsonStdout);
    }

    #[test]
    fn forge_event_spec_exit_code_yields_json_number() {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("exit_code".into(), FieldSource::ExitCode);
        let spec = ForgeEventSpec {
            event_kind: "forge.pr.merged".into(),
            fields,
        };

        let payload = spec.extract_payload(37, None).unwrap();
        assert_eq!(payload.get("exit_code"), Some(&serde_json::json!(37)));
    }

    #[test]
    fn forge_event_spec_empty_fields_yields_empty_object() {
        let spec = ForgeEventSpec {
            event_kind: "forge.pr.merged".into(),
            fields: std::collections::BTreeMap::new(),
        };

        let payload = spec.extract_payload(0, None).unwrap();
        assert!(payload.is_empty());
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
                ..
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

    #[test]
    fn runtime_started_topics_card_only() {
        let ev = Event::RuntimeStarted {
            runtime_id: "rt-1".into(),
            card_id: "card-1".into(),
            kind: crate::runtime::WorkerSessionKind::CodexCard,
            agent_provider: Some(crate::runtime::AgentProvider::Codex),
            status: crate::worker::WorkerSessionState::Starting,
        };
        let t = topics(&ev);
        assert_eq!(t.len(), 2, "topics={t:?}");
        assert!(t.iter().any(|s| s == "card:card-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
        assert!(
            !t.iter().any(|s| s.starts_with("wave:")),
            "topics must not include wave: scope; topics={t:?}"
        );
    }

    #[test]
    fn runtime_status_changed_topics_card_only() {
        let ev = Event::RuntimeStatusChanged {
            runtime_id: "rt-1".into(),
            card_id: "card-1".into(),
            old_status: crate::worker::WorkerSessionState::Starting,
            new_status: crate::worker::WorkerSessionState::Running,
        };
        let t = topics(&ev);
        assert_eq!(t.len(), 2, "topics={t:?}");
        assert!(t.iter().any(|s| s == "card:card-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
        assert!(
            !t.iter().any(|s| s.starts_with("wave:")),
            "topics must not include wave: scope; topics={t:?}"
        );
    }

    #[test]
    fn runtime_superseded_topics_card_only() {
        let ev = Event::RuntimeSuperseded {
            old_runtime_id: "rt-old".into(),
            new_runtime_id: "rt-new".into(),
            card_id: "card-1".into(),
        };
        let t = topics(&ev);
        assert_eq!(t.len(), 2, "topics={t:?}");
        assert!(t.iter().any(|s| s == "card:card-1"), "topics={t:?}");
        assert!(t.iter().any(|s| s == "*"), "topics={t:?}");
        assert!(
            !t.iter().any(|s| s.starts_with("wave:")),
            "topics must not include wave: scope; topics={t:?}"
        );
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
            agent_message: None,
        }
    }

    fn metadata_coverage_events() -> Vec<Event> {
        vec![
            Event::CoveUpdated(cove_sample("cove-updated")),
            Event::CoveDeleted {
                id: CoveId::from("cove-deleted"),
            },
            Event::WaveUpdated(WaveUpdatedPayload::new(
                wave_sample("wave-updated", "cove-1"),
                None,
            )),
            Event::WaveDeleted {
                id: WaveId::from("wave-deleted"),
                cove_id: CoveId::from("cove-1"),
            },
            Event::WaveLifecycleChanged {
                id: WaveId::from("wave-lifecycle"),
                cove_id: CoveId::from("cove-1"),
                from: WaveLifecycle::Draft,
                to: WaveLifecycle::Planning,
                agent_message: None,
            },
            Event::CardAdded(card_sample("card-added", "wave-1")),
            Event::CardUpdated(card_sample("card-updated", "wave-1")),
            Event::CardDeleted {
                id: CardId::from("card-deleted"),
                wave_id: WaveId::from("wave-1"),
            },
            Event::RuntimeStarted {
                runtime_id: "runtime-started".into(),
                card_id: "card-runtime".into(),
                kind: crate::runtime::WorkerSessionKind::CodexCard,
                agent_provider: Some(crate::runtime::AgentProvider::Codex),
                status: crate::worker::WorkerSessionState::Starting,
            },
            Event::RuntimeStatusChanged {
                runtime_id: "runtime-status".into(),
                card_id: "card-runtime".into(),
                old_status: crate::worker::WorkerSessionState::Starting,
                new_status: crate::worker::WorkerSessionState::Running,
            },
            Event::RuntimeSuperseded {
                old_runtime_id: "runtime-old".into(),
                new_runtime_id: "runtime-new".into(),
                card_id: "card-runtime".into(),
            },
            Event::HarnessTranscriptCleared {
                runtime_id: "runtime-transcript".into(),
                card_id: CardId::from("card-runtime"),
                wave_id: WaveId::from("wave-1"),
            },
            Event::HarnessUserMessageEnqueued {
                runtime_id: "runtime-user-message".into(),
                card_id: CardId::from("card-runtime"),
                wave_id: WaveId::from("wave-1"),
                char_count: 5,
            },
            wave_report_edited_sample(),
            Event::OverlaySet(overlay_sample("plugin-1", "card", "card-1", "status")),
            Event::OverlayDeleted {
                plugin_id: "plugin-1".into(),
                entity_kind: "card".into(),
                entity_id: "card-1".into(),
                kind: "status".into(),
            },
            Event::TerminalDeleted {
                id: "terminal-1".into(),
                card_id: CardId::from("card-1"),
            },
            Event::PluginState {
                id: "plugin-1".into(),
                state: "running".into(),
                last_error: None,
            },
            Event::PluginToolRegistered {
                plugin_id: "plugin-1".into(),
                tool_name: "calm.plugin.echo".into(),
            },
            Event::WorkflowRegistered {
                plugin_id: "plugin-1".into(),
                workflow_id: "issue-development".into(),
            },
            Event::CodexHook {
                card_id: CardId::from("card-codex"),
                kind: "hook.codex.stop".into(),
                hook_idempotency_key: "hook-codex".into(),
                payload: serde_json::Value::Null,
            },
            Event::ClaudeHook {
                card_id: CardId::from("card-claude"),
                kind: "hook.claude.stop".into(),
                hook_idempotency_key: "hook-claude".into(),
                payload: serde_json::Value::Null,
            },
            Event::CodexWorkerRequested {
                idempotency_key: "k".into(),
                goal: "g".into(),
                context: serde_json::Value::Null,
                acceptance_criteria: None,
                agent_message: None,
            },
            Event::TerminalWorkerRequested {
                idempotency_key: "k".into(),
                cmd: "ls".into(),
                cwd: None,
                agent_message: None,
            },
            Event::TaskCompleted {
                idempotency_key: "k".into(),
                result: serde_json::Value::Null,
                artifacts: vec![],
                agent_message: None,
            },
            Event::TaskFailed {
                idempotency_key: "k".into(),
                reason: "boom".into(),
                agent_message: None,
            },
            Event::PlanUpdated {
                wave_id: WaveId::from("wave-1"),
                changed_keys: vec!["impl-parser".into()],
                agent_message: None,
            },
            Event::TaskDispatched {
                idempotency_key: "wave-1:impl-parser".into(),
                kind: "codex".into(),
                agent_message: None,
            },
            Event::WorkspaceLeased {
                wave_id: WaveId::from("wave-1"),
                card_id: CardId::from("card-workspace"),
                lease_id: "lease-1".into(),
                path: ".claude/worktrees/wave-1/card-workspace".into(),
            },
            Event::WorkspaceReleased {
                wave_id: WaveId::from("wave-1"),
                card_id: CardId::from("card-workspace"),
                lease_id: "lease-1".into(),
            },
            Event::ForgePrMerged {
                wave_id: WaveId::from("wave-1"),
                subject: ForgeMergeSubject {
                    phase: "impl".into(),
                    slice_id: "6".into(),
                    pr_number: 760,
                },
                head_sha: "head-sha".into(),
                merge_sha: "merge-sha".into(),
            },
            Event::ForgeScanCompleted {
                wave_id: WaveId::from("wave-1"),
                overlapping_prs: vec![1, 2],
            },
            Event::ForgePrOpened {
                wave_id: WaveId::from("wave-1"),
                pr_number: 1,
                head_sha: "head-sha".into(),
            },
            Event::ForgePrDiffRead {
                wave_id: WaveId::from("wave-1"),
                pr_number: 1,
                base_sha: "base-sha".into(),
                head_sha: "head-sha".into(),
                artifact_path: "/tmp/neige/forge-diff.patch".into(),
            },
            Event::ForgePrChecks {
                wave_id: WaveId::from("wave-1"),
                pr_number: 1,
                conclusion: "success".into(),
            },
            Event::ForgeIssueClosed {
                wave_id: WaveId::from("wave-1"),
                issue_number: 1,
            },
            Event::WorktreeProvisioned {
                wave_id: WaveId::from("wave-1"),
                card_id: CardId::from("card-worktree"),
                path: "/tmp/worktree".into(),
            },
            Event::WorktreeRemoved {
                wave_id: WaveId::from("wave-1"),
                card_id: CardId::from("card-worktree"),
                path: "/tmp/worktree".into(),
            },
        ]
    }

    fn cove_sample(id: &str) -> Cove {
        Cove {
            id: CoveId::from(id),
            name: "n".into(),
            color: "#fff".into(),
            sort: 1.0,
            kind: crate::model::CoveKind::User,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn wave_sample(id: &str, cove_id: &str) -> Wave {
        Wave {
            id: WaveId::from(id),
            cove_id: CoveId::from(cove_id),
            title: "t".into(),
            sort: 1.0,
            archived_at: None,
            pinned_at: None,
            lifecycle: WaveLifecycle::Draft,
            cwd: String::new(),
            workflow_id: None,
            terminal_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn card_sample(id: &str, wave_id: &str) -> Card {
        Card {
            id: CardId::from(id),
            wave_id: WaveId::from(wave_id),
            kind: "terminal".into(),
            sort: 1.0,
            payload: serde_json::json!({}),
            runtime: None,
            deletable: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn overlay_sample(plugin_id: &str, entity_kind: &str, entity_id: &str, kind: &str) -> Overlay {
        Overlay {
            id: "overlay-1".into(),
            plugin_id: plugin_id.into(),
            entity_kind: entity_kind.into(),
            entity_id: entity_id.into(),
            kind: kind.into(),
            payload: serde_json::json!({}),
            updated_at: 0,
        }
    }

    #[test]
    fn new_variants_round_trip_via_from_kind_and_payload() {
        // Replay path: `Event::from_kind_and_payload` reconstitutes a
        // typed Event from the `(kind, payload)` columns. Pin that the
        // PR4 variants survive this path so the eventual sync-engine
        // replay doesn't strand them.
        for (kind, expected_kind, payload) in [
            (
                "claude.hook",
                "claude.hook",
                serde_json::json!({
                    "card_id": "card-1",
                    "kind": "hook.claude.stop",
                    "payload": {},
                }),
            ),
            (
                "codex.worker_requested",
                "codex.worker_requested",
                serde_json::json!({
                    "idempotency_key": "k",
                    "goal": "g",
                    "context": {},
                }),
            ),
            (
                "codex.job_requested",
                "codex.worker_requested",
                serde_json::json!({
                    "idempotency_key": "k",
                    "goal": "g",
                    "context": {},
                }),
            ),
            (
                "terminal.worker_requested",
                "terminal.worker_requested",
                serde_json::json!({ "idempotency_key": "k", "cmd": "ls" }),
            ),
            (
                "terminal.job_requested",
                "terminal.worker_requested",
                serde_json::json!({ "idempotency_key": "k", "cmd": "ls" }),
            ),
            (
                "task.completed",
                "task.completed",
                serde_json::json!({
                    "idempotency_key": "k",
                    "result": {},
                    "artifacts": [],
                }),
            ),
            (
                "task.failed",
                "task.failed",
                serde_json::json!({ "idempotency_key": "k", "reason": "r" }),
            ),
            (
                "runtime.started",
                "runtime.started",
                serde_json::json!({
                    "runtime_id": "runtime-1",
                    "card_id": "card-1",
                    "kind": "codex",
                    "agent_provider": "codex",
                    "status": "starting",
                }),
            ),
            (
                "runtime.status_changed",
                "runtime.status_changed",
                serde_json::json!({
                    "runtime_id": "runtime-1",
                    "card_id": "card-1",
                    "old_status": "starting",
                    "new_status": "running",
                }),
            ),
            (
                "runtime.superseded",
                "runtime.superseded",
                serde_json::json!({
                    "old_runtime_id": "runtime-1",
                    "new_runtime_id": "runtime-2",
                    "card_id": "card-1",
                }),
            ),
            (
                "workspace.leased",
                "workspace.leased",
                serde_json::json!({
                    "wave_id": "wave-1",
                    "card_id": "card-1",
                    "lease_id": "lease-1",
                    "path": ".claude/worktrees/wave-1/card-1",
                }),
            ),
            (
                "workspace.released",
                "workspace.released",
                serde_json::json!({
                    "wave_id": "wave-1",
                    "card_id": "card-1",
                    "lease_id": "lease-1",
                }),
            ),
            (
                "forge.pr.merged",
                "forge.pr.merged",
                serde_json::json!({
                    "wave_id": "wave-1",
                    "subject": {
                        "phase": "impl",
                        "slice_id": "6",
                        "pr_number": 760,
                    },
                    "head_sha": "head-sha",
                    "merge_sha": "merge-sha",
                }),
            ),
            (
                "forge.scan.completed",
                "forge.scan.completed",
                serde_json::json!({
                    "wave_id": "wave-1",
                    "overlapping_prs": [1, 2],
                }),
            ),
            (
                "forge.pr.opened",
                "forge.pr.opened",
                serde_json::json!({
                    "wave_id": "wave-1",
                    "pr_number": 1,
                    "head_sha": "head-sha",
                }),
            ),
            (
                "forge.pr.diff.read",
                "forge.pr.diff.read",
                serde_json::json!({
                    "wave_id": "wave-1",
                    "pr_number": 1,
                    "base_sha": "base-sha",
                    "head_sha": "head-sha",
                    "artifact_path": "/tmp/neige/forge-diff.patch",
                }),
            ),
            (
                "forge.pr.checks",
                "forge.pr.checks",
                serde_json::json!({
                    "wave_id": "wave-1",
                    "pr_number": 1,
                    "conclusion": "success",
                }),
            ),
            (
                "forge.issue.closed",
                "forge.issue.closed",
                serde_json::json!({
                    "wave_id": "wave-1",
                    "issue_number": 1,
                }),
            ),
            (
                "worktree.provisioned",
                "worktree.provisioned",
                serde_json::json!({
                    "wave_id": "wave-1",
                    "card_id": "card-1",
                    "path": "/tmp/worktree",
                }),
            ),
            (
                "worktree.removed",
                "worktree.removed",
                serde_json::json!({
                    "wave_id": "wave-1",
                    "card_id": "card-1",
                    "path": "/tmp/worktree",
                }),
            ),
        ] {
            let ev = Event::from_kind_and_payload(kind, payload)
                .unwrap_or_else(|e| panic!("replay decode failed for {kind}: {e}"));
            assert_eq!(ev.kind_tag(), expected_kind, "round-trip kind mismatch");
            match kind {
                "codex.job_requested" => {
                    assert!(matches!(ev, Event::CodexWorkerRequested { .. }))
                }
                "terminal.job_requested" => {
                    assert!(matches!(ev, Event::TerminalWorkerRequested { .. }))
                }
                _ => {}
            }
        }
    }
}
