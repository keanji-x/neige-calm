// Wire-shape types: thin re-export shim over `generated.ts` (REST) and
// `generated-events.ts` (WS event envelopes).
//
// `generated.ts` is produced from the kernel's OpenAPI spec (the source of
// truth for the kernel ↔ UI REST contract). `generated-events.ts` is
// produced from the Rust `Event` enum via `ts-rs` (the source of truth for
// the WS `/api/events` envelope). Run `npm run gen:api` to refresh both
// after backend changes — it shells out to `cargo test export_bindings_`
// (writes `generated-events.ts`) and `cargo run --bin emit-openapi |
// openapi-typescript` (writes `generated.ts`). The intermediate
// `openapi.json` is committed so frontend builds don't require cargo.
//
// This file exists to keep the public type names (KernelCove, KernelWave,
// ...) stable for consumers — only the underlying definition migrated from
// hand-maintained to generated. A few shapes intentionally diverge from the
// generated form where the OpenAPI emission lost fidelity:
//
//   * `payload: serde_json::Value` comes through as `Record<string, never>`
//     (basically "empty object"), which is too narrow for the actual blobs
//     the kernel passes around. We override it to `unknown` to match the
//     previous hand-rolled wire and keep `cards/*`, `useKernel`, and
//     `adapt.ts` working without per-call casts.
//   * `entity_kind` on `NewOverlayBody` keeps its `'wave' | 'card'` literal
//     union — utoipa emitted the underlying Rust `String` as plain `string`.
//
// WS event-envelope types now come from `./generated-events.ts` (re-exported
// here as `WireEvent` for backwards compatibility). The hand-written union
// was deleted in D7 (issue #5). The runtime zod schemas in `./schemas.ts`
// are pinned to the generated `Event` type via an `expectTypeOf` conformance
// test in `./schemas.test.ts` — any drift between the Rust enum and the
// zod validator fails at type-check time.

import type { components } from './generated';
import type { Event as GeneratedEvent } from './generated-events';

type Schemas = components['schemas'];

// ---------------- Domain models (REST responses) ----------------

export type KernelCove = Schemas['Cove'];
export type KernelWave = Schemas['Wave'];

/**
 * Override `payload` to `unknown` — the OpenAPI emitter renders Rust's
 * `serde_json::Value` as `Record<string, never>` (empty object), which is
 * unusable for the heterogeneous blobs kernel cards actually carry.
 */
export type KernelCard = Omit<Schemas['Card'], 'payload'> & { payload: unknown };

export type KernelOverlay = Omit<Schemas['Overlay'], 'payload'> & { payload: unknown };

export type KernelTerminal = Schemas['Terminal'];

export type KernelPlugin = Schemas['Plugin'];

export type KernelWaveDetail = Omit<Schemas['WaveDetail'], 'cards' | 'overlays'> & {
  cards: KernelCard[];
  overlays: KernelOverlay[];
};

// ---------------- Event envelope (WS `/api/events`) ----------------
//
// Not in OpenAPI — WS endpoints don't surface as REST request/response
// pairs. Source of truth is the Rust `Event` enum in
// `crates/calm-server/src/event.rs`; `ts-rs` emits the TS union into
// `./generated-events.ts`. Runtime parse + narrowing lives in `./schemas.ts`
// (`wireEventSchema`), and `./schemas.test.ts` pins the zod schema's inferred
// type to `Event` at compile time.
//
// Re-exported here as `WireEvent` to keep existing consumers
// (`api/events.ts`, `app/eventBridge.tsx`, `cards/*`) working without
// touching their imports.

export type WireEvent = GeneratedEvent;

// ---------------- Request DTOs (mirror `model::NewX` / `XPatch`) ----------------

export type NewCoveBody   = Schemas['NewCove'];
export type CovePatchBody = Schemas['CovePatch'];

export type NewWaveBody   = Schemas['NewWave'];
export type WavePatchBody = Schemas['WavePatch'];

/**
 * `payload` override mirrors `KernelCard`: kernel routes accept any JSON
 * blob; the generated `Record<string, never>` would force callers to cast.
 */
export type NewCardBody   = Omit<Schemas['NewCard'], 'payload'> & { payload?: unknown };
export type CardPatchBody = Omit<Schemas['CardPatch'], 'payload'> & { payload?: unknown };

/**
 * `entity_kind` keeps its literal union — the Rust side is `String` so utoipa
 * emits `string`, but the kernel today only addresses overlays scoped to
 * `"wave"`, `"card"`, or `"view"` (the latter introduced for Scope E's
 * `useOverlayState` + WaveGrid layout, per design doc §5). New entity kinds
 * land here in lock-step with a matching server-side acceptance.
 */
export type NewOverlayBody = Omit<Schemas['NewOverlay'], 'entity_kind' | 'payload'> & {
  entity_kind: 'wave' | 'card' | 'view';
  payload: unknown;
};

export type NewTerminalBody = Schemas['NewTerminalBody'];

export type NewCodexBody = Schemas['NewCodexBody'];

// ---------------- fs ----------------
//
// Used by the DirectoryPicker widget that backs the codex `cwd` field.
export type ListdirResponse = Schemas['ListdirResponse'];
export type DirEntry = Schemas['DirEntry'];

// ---------------- settings ----------------
//
// App-global string-bag persisted under `settings.<key>`. The kernel uses
// `http_proxy` / `https_proxy` today; any string keys are accepted.
export type SettingsBag = Schemas['SettingsBag'];
export type SettingsPutBody = Schemas['SettingsPutBody'];
