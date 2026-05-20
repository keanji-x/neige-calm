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

/** `model::Cove` — cove metadata row. */
export const coveSchema = z.object({
  id: z.string(),
  name: z.string(),
  color: z.string(),
  sort: z.number(),
  created_at: z.number(),
  updated_at: z.number(),
});

/** `model::Wave` — wave metadata row. `archived_at` is `Option<i64>` server-side. */
export const waveSchema = z.object({
  id: z.string(),
  cove_id: z.string(),
  title: z.string(),
  sort: z.number(),
  archived_at: z.number().nullable(),
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
  cardAddedSchema,
  cardUpdatedSchema,
  cardDeletedSchema,
  overlaySetSchema,
  overlayDeletedSchema,
  pluginStateSchema,
  codexHookSchema,
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
export type CardAddedEvent = z.infer<typeof cardAddedSchema>;
export type CardUpdatedEvent = z.infer<typeof cardUpdatedSchema>;
export type CardDeletedEvent = z.infer<typeof cardDeletedSchema>;
export type OverlaySetEvent = z.infer<typeof overlaySetSchema>;
export type OverlayDeletedEvent = z.infer<typeof overlayDeletedSchema>;
export type PluginStateEvent = z.infer<typeof pluginStateSchema>;
export type CodexHookEvent = z.infer<typeof codexHookSchema>;

export type WireEvent = z.infer<typeof wireEventSchema>;
