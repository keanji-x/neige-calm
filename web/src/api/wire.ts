// Wire-shape types: a 1:1 mirror of `crates/calm-server/src/model.rs` JSON.
// Keep this file in sync with the Rust definitions — it's the contract
// boundary between kernel and UI.

export interface KernelCove {
  id: string;
  name: string;
  color: string;
  sort: number;
  created_at: number;
  updated_at: number;
}

export interface KernelWave {
  id: string;
  cove_id: string;
  title: string;
  sort: number;
  archived_at: number | null;
  created_at: number;
  updated_at: number;
}

export interface KernelCard {
  id: string;
  wave_id: string;
  /** `"terminal"` | `"ui://<plugin-id>/<view-id>"`. Kernel does not interpret further. */
  kind: string;
  sort: number;
  /** Opaque per-card-kind blob; for built-in `terminal` cards it's empty. */
  payload: unknown;
  created_at: number;
  updated_at: number;
}

export interface KernelOverlay {
  id: string;
  plugin_id: string;
  /** `"wave"` | `"card"`. */
  entity_kind: string;
  entity_id: string;
  /** Plugin-defined string. */
  kind: string;
  payload: unknown;
  updated_at: number;
}

export interface KernelTerminal {
  id: string;
  card_id: string;
  program: string;
  cwd: string;
  env: Record<string, string>;
  daemon_handle: string | null;
  created_at: number;
}

export interface KernelWaveDetail {
  wave: KernelWave;
  cards: KernelCard[];
  overlays: KernelOverlay[];
}

// ---------------- Event envelope (WS `/api/events`) ----------------

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

export interface NewCoveBody  { name: string; color: string; sort?: number }
export interface CovePatchBody { name?: string; color?: string; sort?: number }

export interface NewWaveBody  { cove_id: string; title: string; sort?: number }
export interface WavePatchBody {
  title?: string;
  sort?: number;
  /** Pass `null` to unarchive, a number to archive at that ts, undefined to leave alone. */
  archived_at?: number | null;
}

export interface NewCardBody  {
  /** Server overrides from the path param `:wave_id`. */
  wave_id?: string;
  kind: string;
  sort?: number;
  payload?: unknown;
}
export interface CardPatchBody { kind?: string; sort?: number; payload?: unknown }

export interface NewOverlayBody {
  plugin_id: string;
  entity_kind: 'wave' | 'card';
  entity_id: string;
  kind: string;
  payload: unknown;
}

export interface NewTerminalBody {
  /** Empty string ⇒ kernel uses `$SHELL`. */
  program?: string;
  /** Empty string ⇒ kernel uses `$HOME`. */
  cwd?: string;
  env?: Record<string, string>;
}
