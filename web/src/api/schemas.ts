// Zod schemas for WS `/api/events` payloads. The source of truth on the
// frontend for what the kernel can emit. Mirrors
// `crates/calm-server/src/event.rs`'s `Event` enum (serde-tagged
// `{ ev, data }`) and `crates/calm-server/src/model.rs`'s entity types.
//
// On parse failure, callers should log + skip dispatch — never throw — so a
// new event variant added server-side doesn't crash the UI. The runtime
// check exists to catch *unexpected* payload shapes (schema drift, partial
// rollouts, broken proxies) rather than to police every field.
//
// Inferred TS types are re-exported so consumers can opt in without touching
// the legacy `wire.ts` `WireEvent` union. The two are intentionally parallel
// today — a future migration can collapse them once all consumers move over.

import { z } from 'zod';

// ---------------- Entity schemas (mirror model.rs) ----------------

/**
 * PR3 of #136 — `model::CardRole`. The server-side authorization label
 * persisted on `cards.role`. Frontend doesn't consume this today; the
 * schema is exported so a future UI surface (PR6+ may surface "this is
 * the wave's spec card" indicators) can validate against the same wire
 * shape without a duplicate definition.
 *
 * Wire values are lowercase via `#[serde(rename_all = "lowercase")]` on
 * the Rust enum. ts-rs will emit the matching union in
 * `generated-events.ts`; this zod schema is the runtime validator.
 */
export const cardRoleSchema = z.enum(['plain', 'spec', 'worker']);
export type CardRole = z.infer<typeof cardRoleSchema>;

/**
 * Issue #175 — `model::CoveKind`. Marks whether a cove is part of the
 * user-visible workspace (`'user'`) or is the kernel-owned singleton that
 * hosts the default Today terminal's wave (`'system'`). The kernel already
 * filters `kind='system'` out of `GET /api/coves` by default, so this
 * frontend schema's main job is to type the field for the optional
 * belt-and-suspenders `.filter(c => c.kind === 'user')` in CalmApp /
 * router. Defaults to `'user'` so pre-#175 wire payloads (event-log
 * replay, legacy fixtures) round-trip without forcing a fixture rewrite.
 */
export const coveKindSchema = z.enum(['user', 'system']).default('user');
export type CoveKind = z.infer<typeof coveKindSchema>;

/** `model::Cove` — cove metadata row. */
export const coveSchema = z.object({
  id: z.string(),
  name: z.string(),
  color: z.string(),
  sort: z.number(),
  kind: coveKindSchema,
  created_at: z.number(),
  updated_at: z.number(),
});

/**
 * Issue #145 — `model::WaveLifecycle`. Single source of truth for the
 * lifecycle state machine the Spec Agent drives. Wire values are
 * lowercase (`#[serde(rename_all = "lowercase")]` on the Rust enum).
 * `archived` is intentionally NOT a lifecycle state — archive is
 * orthogonal visibility on `wave.archived_at`.
 *
 * Defaults to `'draft'` for any pre-#145 wire payload (replay
 * fixtures, legacy event logs) — matches the DB DEFAULT in
 * migration 0012 and the `#[serde(default)]` on the Rust struct
 * field. Forces wave payloads emitted *before* the lifecycle column
 * existed to parse without a fixture rewrite.
 */
export const waveLifecycleSchema = z
  .enum([
    'draft',
    'planning',
    'dispatching',
    'working',
    'blocked',
    'reviewing',
    'done',
    'canceled',
    'failed',
  ])
  .default('draft');
export type WaveLifecycle = z.infer<typeof waveLifecycleSchema>;

/** `model::Wave` — wave metadata row. `archived_at` is `Option<i64>` server-side. */
export const waveSchema = z.object({
  id: z.string(),
  cove_id: z.string(),
  title: z.string(),
  sort: z.number(),
  archived_at: z.number().nullable(),
  /**
   * Issue #145 — the wave's lifecycle state. Defaulted at the schema
   * layer to `'draft'` so a missing field on pre-#145 wire payloads
   * (event-log replay fixtures) parses cleanly. The kernel always
   * stamps a value on fresh writes.
   */
  lifecycle: waveLifecycleSchema,
  created_at: z.number(),
  updated_at: z.number(),
});

/** `model::Card` — card row. `payload` is opaque `serde_json::Value`. */
export const cardSchema = z.object({
  id: z.string(),
  wave_id: z.string(),
  kind: z.string(),
  sort: z.number(),
  // serde_json::Value on the wire: arbitrary JSON. Kernel never inspects.
  payload: z.unknown(),
  created_at: z.number(),
  updated_at: z.number(),
});

/** `model::Overlay` — plugin overlay row. `payload` is opaque JSON. */
export const overlaySchema = z.object({
  id: z.string(),
  plugin_id: z.string(),
  // Documented as "wave" | "card" but kept open for forward-compat.
  entity_kind: z.string(),
  entity_id: z.string(),
  kind: z.string(),
  payload: z.unknown(),
  updated_at: z.number(),
});

// ---------------- Event schemas (mirror event.rs) ----------------

export const coveUpdatedSchema = z.object({
  ev: z.literal('cove.updated'),
  data: coveSchema,
});

export const coveDeletedSchema = z.object({
  ev: z.literal('cove.deleted'),
  data: z.object({ id: z.string() }),
});

export const waveUpdatedSchema = z.object({
  ev: z.literal('wave.updated'),
  data: waveSchema,
});

export const waveDeletedSchema = z.object({
  ev: z.literal('wave.deleted'),
  data: z.object({ id: z.string(), cove_id: z.string() }),
});

/**
 * Issue #145 — `Event::WaveLifecycleChanged`. Emitted exactly once per
 * validated `from → to` transition. Reducers downstream can subscribe to
 * `kind = wave.lifecycle_changed` directly without inspecting every
 * `wave.updated` for a possibly-unchanged `lifecycle` field. Wave-scoped
 * (routes to `wave:<id>` and `cove:<cove>` topics).
 */
export const waveLifecycleChangedSchema = z.object({
  ev: z.literal('wave.lifecycle_changed'),
  data: z.object({
    id: z.string(),
    cove_id: z.string(),
    from: waveLifecycleSchema,
    to: waveLifecycleSchema,
  }),
});

export const cardAddedSchema = z.object({
  ev: z.literal('card.added'),
  data: cardSchema,
});

export const cardUpdatedSchema = z.object({
  ev: z.literal('card.updated'),
  data: cardSchema,
});

export const cardDeletedSchema = z.object({
  ev: z.literal('card.deleted'),
  data: z.object({ id: z.string(), wave_id: z.string() }),
});

export const overlaySetSchema = z.object({
  ev: z.literal('overlay.set'),
  data: overlaySchema,
});

export const overlayDeletedSchema = z.object({
  ev: z.literal('overlay.deleted'),
  data: z.object({
    plugin_id: z.string(),
    entity_kind: z.string(),
    entity_id: z.string(),
    kind: z.string(),
  }),
});

/**
 * `Event::TerminalDeleted` — emitted by the orphan-terminal sweeper
 * (`crates/calm-server/src/terminal_sweeper.rs`) when a terminal row is
 * reaped because no card payload references it anymore. Actor is
 * `"kernel"` on the events-table row. Topic: `terminal:<id>`. The UI
 * doesn't currently subscribe to per-terminal topics, but the schema is
 * carried here so the runtime validator accepts the frame on the
 * firehose (`*`) subscription without dispatch-mismatch warnings.
 */
export const terminalDeletedSchema = z.object({
  ev: z.literal('terminal.deleted'),
  data: z.object({
    id: z.string(),
    card_id: z.string(),
  }),
});

/**
 * `Event::PluginState` — emitted by the plugin host on lifecycle transitions.
 * `state` is a free-form string (e.g. `"Spawning"`, `"Running"`, `"Crashed"`)
 * matching the Rust `PluginState` enum's `Display`. `last_error` is `None`
 * for healthy transitions and `Some(msg)` on crash / init-rejected paths
 * (skipped from serialization when `None`, so the field is optional here).
 */
export const pluginStateSchema = z.object({
  ev: z.literal('plugin.state'),
  data: z.object({
    id: z.string(),
    state: z.string(),
    last_error: z.string().optional(),
  }),
});

/**
 * `Event::CodexHook` — passthrough of one codex-CLI hook firing
 * (PreToolUse / PostToolUse / Stop / ...). `kind` carries a snake-case
 * discriminator (`hook.codex.<event>`) so callers can pattern-match
 * without typing every codex payload field. `payload` is the raw codex
 * JSON, kept opaque.
 */
export const codexHookSchema = z.object({
  ev: z.literal('codex.hook'),
  data: z.object({
    card_id: z.string(),
    kind: z.string(),
    payload: z.unknown(),
  }),
});

// ---------------- PR4 of #136: dispatcher + task-lifecycle variants ----
//
// Schema-only PR — no kernel emitters today. PR5 (Dispatcher) and PR8
// (wait_for_events) wire them. The four schemas below pin the wire shape
// the kernel will start emitting once PR5 lands, so the runtime
// validator at the WS boundary doesn't drop frames on the floor.
//
// `ArtifactRef` is a transparent newtype on the server (#129 placeholder);
// ts-rs emits `export type ArtifactRef = string;` so on the wire each
// element of `task.completed.artifacts[]` is a bare string.

/**
 * `Event::CodexJobRequested` — spec/worker card asks the kernel
 * dispatcher to spawn a codex worker card. PR5's `Dispatcher` consumes
 * via `EventBus::subscribe(kinds=["*.requested"])`; PR8's
 * `wait_for_events` correlates the eventual `task.completed` /
 * `task.failed` back to the requester via `idempotency_key`.
 *
 * `context` is opaque `serde_json::Value` (working-dir hints, prior turn
 * history, model preference) — kernel never inspects, dispatcher
 * forwards verbatim into the spawned worker's card payload.
 */
export const codexJobRequestedSchema = z.object({
  ev: z.literal('codex.job_requested'),
  data: z.object({
    idempotency_key: z.string(),
    goal: z.string(),
    context: z.unknown(),
    acceptance_criteria: z.string().optional(),
  }),
});

/**
 * `Event::TerminalJobRequested` — spec card asks the dispatcher to spawn
 * a terminal worker card. `cwd` is `None` when the spec card defers to
 * the wave/cove default working directory.
 */
export const terminalJobRequestedSchema = z.object({
  ev: z.literal('terminal.job_requested'),
  data: z.object({
    idempotency_key: z.string(),
    cmd: z.string(),
    cwd: z.string().optional(),
  }),
});

/**
 * `Event::TaskCompleted` — worker card reports task completion. PR8's
 * `wait_for_events` delivers this to the requesting spec card.
 * `idempotency_key` echoes the matching `*.job_requested` key so the
 * spec can correlate without parsing the worker card's identity.
 *
 * `artifacts` is `Vec<ArtifactRef>` server-side; `ArtifactRef` is a
 * transparent newtype around `String`, so each element is a bare string
 * on the wire. #129 will expand the type with hash / content-type /
 * storage-uri — at that point this schema will tighten alongside.
 */
export const taskCompletedSchema = z.object({
  ev: z.literal('task.completed'),
  data: z.object({
    idempotency_key: z.string(),
    result: z.unknown(),
    artifacts: z.array(z.string()),
  }),
});

/**
 * `Event::TaskFailed` — worker card reports task failure. `reason` is a
 * free-form failure string; the kernel never parses it but persists it
 * on the events table so audit-log replay can surface the rationale the
 * worker gave its spec.
 */
export const taskFailedSchema = z.object({
  ev: z.literal('task.failed'),
  data: z.object({
    idempotency_key: z.string(),
    reason: z.string(),
  }),
});

// ---------------- EventScope (mirror event.rs) ----------------

/**
 * `EventScope` — the event's "home scope" in the cove → wave → card
 * hierarchy. PR2 of #136 adds this to every persisted event so future
 * MCP subscribers / dispatcher routes can filter without re-parsing
 * the payload. Tagged `{kind, id}` shape via `#[serde(tag, content)]`
 * on the Rust side.
 *
 * `System` is the catch-all for events that genuinely don't belong to
 * a single cove/wave/card (`plugin.state`, cove-create, the pre-PR2
 * NULL-fallback). Pre-PR2 history rows replay as `System`.
 */
export const eventScopeSchema = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('System') }),
  z.object({ kind: z.literal('Cove'), id: z.object({ cove: z.string() }) }),
  z.object({
    kind: z.literal('Wave'),
    id: z.object({ wave: z.string(), cove: z.string() }),
  }),
  z.object({
    kind: z.literal('Card'),
    id: z.object({ card: z.string(), wave: z.string(), cove: z.string() }),
  }),
]);

export type EventScope = z.infer<typeof eventScopeSchema>;

// ---------------- Discriminated union ----------------

/**
 * The complete set of events the kernel can push on `/api/events`. Keep this
 * 1:1 with `event::Event` in calm-server; the WS handler runtime-validates
 * each frame through this schema and skips dispatch on mismatch.
 */
export const wireEventSchema = z.discriminatedUnion('ev', [
  coveUpdatedSchema,
  coveDeletedSchema,
  waveUpdatedSchema,
  waveDeletedSchema,
  waveLifecycleChangedSchema,
  cardAddedSchema,
  cardUpdatedSchema,
  cardDeletedSchema,
  overlaySetSchema,
  overlayDeletedSchema,
  terminalDeletedSchema,
  pluginStateSchema,
  codexHookSchema,
  codexJobRequestedSchema,
  terminalJobRequestedSchema,
  taskCompletedSchema,
  taskFailedSchema,
]);

// ---------------- Inferred types ----------------
//
// Available for consumers that want a stronger type than `WireEvent` from
// `wire.ts`. Not migrated yet — the two coexist by design until a sweep.

export type Cove = z.infer<typeof coveSchema>;
export type Wave = z.infer<typeof waveSchema>;
export type Card = z.infer<typeof cardSchema>;
export type Overlay = z.infer<typeof overlaySchema>;

export type CoveUpdatedEvent = z.infer<typeof coveUpdatedSchema>;
export type CoveDeletedEvent = z.infer<typeof coveDeletedSchema>;
export type WaveUpdatedEvent = z.infer<typeof waveUpdatedSchema>;
export type WaveDeletedEvent = z.infer<typeof waveDeletedSchema>;
export type WaveLifecycleChangedEvent = z.infer<typeof waveLifecycleChangedSchema>;
export type CardAddedEvent = z.infer<typeof cardAddedSchema>;
export type CardUpdatedEvent = z.infer<typeof cardUpdatedSchema>;
export type CardDeletedEvent = z.infer<typeof cardDeletedSchema>;
export type OverlaySetEvent = z.infer<typeof overlaySetSchema>;
export type OverlayDeletedEvent = z.infer<typeof overlayDeletedSchema>;
export type TerminalDeletedEvent = z.infer<typeof terminalDeletedSchema>;
export type PluginStateEvent = z.infer<typeof pluginStateSchema>;
export type CodexHookEvent = z.infer<typeof codexHookSchema>;
export type CodexJobRequestedEvent = z.infer<typeof codexJobRequestedSchema>;
export type TerminalJobRequestedEvent = z.infer<typeof terminalJobRequestedSchema>;
export type TaskCompletedEvent = z.infer<typeof taskCompletedSchema>;
export type TaskFailedEvent = z.infer<typeof taskFailedSchema>;

export type WireEvent = z.infer<typeof wireEventSchema>;
