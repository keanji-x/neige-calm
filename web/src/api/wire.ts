// Wire-shape types: thin re-export shim over `generated.ts`.
//
// `generated.ts` is produced from the kernel's OpenAPI spec (the source of
// truth for the kernel ↔ UI JSON contract). Run `npm run gen:api` to refresh
// after backend changes — it shells out to `cargo run --bin emit-openapi`
// and pipes the result through `openapi-typescript`. The intermediate
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
// WS event-envelope types (`WireEvent`) are kept here as-is: they're not in
// the OpenAPI spec (no JSON request/response pair) and the runtime zod
// schemas live in `./schemas.ts`.

import type { components } from './generated';

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
// pairs. Runtime parse + narrowing lives in `./schemas.ts` (`wireEventSchema`).

/**
 * Wire event from the kernel. Mirrors `crates/calm-server/src/event.rs`
 * `#[serde(tag = "ev", content = "data")]`.
 *
 * Subscribers should narrow by `ev` then read `data` (the TS exhaustiveness
 * check in `dispatchEvent()` will flag any missing variant).
 */
export type WireEvent =
  | { ev: 'cove.updated';    data: KernelCove }
  | { ev: 'cove.deleted';    data: { id: string } }
  | { ev: 'wave.updated';    data: KernelWave }
  | { ev: 'wave.deleted';    data: { id: string; cove_id: string } }
  | { ev: 'card.added';      data: KernelCard }
  | { ev: 'card.updated';    data: KernelCard }
  | { ev: 'card.deleted';    data: { id: string; wave_id: string } }
  | { ev: 'overlay.set';     data: KernelOverlay }
  | {
      ev: 'overlay.deleted';
      data: {
        plugin_id: string;
        entity_kind: string;
        entity_id: string;
        kind: string;
      };
    }
  | { ev: 'plugin.state';    data: { id: string; state: string } };

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
 * emits `string`, but the kernel only accepts `"wave"` or `"card"`.
 */
export type NewOverlayBody = Omit<Schemas['NewOverlay'], 'entity_kind' | 'payload'> & {
  entity_kind: 'wave' | 'card';
  payload: unknown;
};

export type NewTerminalBody = Schemas['NewTerminalBody'];
