// Card-type registry.
//
// The kernel dispatches every Card through `kind: string`; the UI maps that
// string (via `api/adapt.ts`) to a discriminated `WaveCardData` variant and
// renders the right component. Before M3 those three step lookups were three
// 5-case switches scattered across `ui.tsx`, `WaveGrid.tsx`, and
// `api/adapt.ts`. This module collapses them into one `Map<type, CardEntry>`
// so plugin entries (Slice F) can `.set()` themselves into the same dispatch
// table at runtime without the dispatcher caring.
//
// Built-ins register at app boot via `registerBuiltins()` from
// `cards/builtins/index.ts`; plugin cards will register lazily as their
// iframes mount.
//
// Plugin card kinds: post-M4 the registry only accepts the canonical
// `ui://<plugin>/<view>` resource URI. The legacy `plugin:<id>:<view>` form
// is rejected — `PluginIframeEntry.fromKernel` returns null on it, so the
// adapter falls through and `renderCard` logs a one-shot warning.

import { createElement, type FC, type ReactNode } from 'react';
import type { WaveCardData } from '../types';
import type { KernelCard } from '../api/wire';

export interface CardSize {
  w: number;
  h: number;
  minW: number;
  minH: number;
}

/**
 * Minimal JSON-Schema subset the bundled `SchemaForm` renders. Intentionally
 * not the full JSON-Schema spec — we only need enough to drive the create
 * dialog for built-in card kinds. Plugins requiring richer schemas should
 * carry their own renderer through the plugin host.
 */
export interface CreateField {
  /** Field key on the resulting body object. */
  key: string;
  /** Label rendered above the input. */
  label: string;
  /**
   * Storage type — controls the rendered widget.
   *
   * `'directory'` / `'file'` swap the plain text input for
   * `DirectoryPicker`: a read-only field with a browser that walks the
   * host filesystem via `GET /api/fs/listdir`. Picked for codex's `cwd`
   * and the file-viewer card path so users don't have to remember
   * absolute paths.
   */
  type: 'string' | 'textarea' | 'enum' | 'directory' | 'file';
  /** Required for `type: 'enum'`. */
  options?: string[];
  /** Default value pre-filled on first render. */
  default?: string;
  /** Optional placeholder shown when the field is empty. */
  placeholder?: string;
  /** True forces the input non-empty before the form will submit. */
  required?: boolean;
}

export interface CreateSchema {
  /** Ordered field list — rendered top-to-bottom. */
  fields: CreateField[];
}

/** Common props every built-in card component receives. Cards must forward
 *  `onClose` (when provided) to `<CardHead>` so the X button renders inside
 *  the head. Optional: contexts that own the close affordance elsewhere
 *  (e.g. WaveList's row-level button) simply pass `undefined`. */
export interface CardComponentProps<T extends WaveCardData = WaveCardData> {
  card: T;
  onClose?: () => void;
}

export interface CardEntry<T extends WaveCardData = WaveCardData> {
  /** The discriminator value used in `T['type']`, e.g. `'terminal'`, `'doc'`,
   *  or the sentinel `'plugin'` for `ui://`-backed iframe cards. */
  type: T['type'] | string;
  Component: FC<CardComponentProps<T>>;
  defaultSize: CardSize;
  /** Optional — kernel→UI adaptation. Receives the raw KernelCard;
   *  return null if this entry doesn't claim that kernel card. */
  fromKernel?: (k: KernelCard) => T | null;
  /** Optional — when present, the entry appears in the AddPanel menu.
   *  Slice G iterates this. */
  addPanel?: {
    label: string;
    /** When present, picking this entry from the AddPanel menu shows an
     *  inline config card rendered by `SchemaForm` instead of immediately
     *  creating the card. Omit for zero-config kinds (current terminal). */
    createSchema?: CreateSchema;
  };
}

const REGISTRY = new Map<string, CardEntry<WaveCardData>>();

/** Fallback size for unknown card types. Sane mid-range default that fits
 *  any of the built-in shapes; we'd rather render a slightly-wrong-sized
 *  placeholder than throw. */
const FALLBACK_SIZE: CardSize = { w: 4, h: 6, minW: 3, minH: 3 };

const warned = new Set<string>();
function warnOnce(key: string, msg: string) {
  if (warned.has(key)) return;
  warned.add(key);
  // eslint-disable-next-line no-console
  console.warn(msg);
}

export function registerCard<T extends WaveCardData>(entry: CardEntry<T>): void {
  // The cast is the price of letting one Map hold heterogeneous entries.
  // Callers see the typed `CardEntry<T>`; the map stores the erased shape.
  REGISTRY.set(entry.type, entry as unknown as CardEntry<WaveCardData>);
}

export function renderCard(
  card: WaveCardData,
  opts: { onClose?: () => void } = {},
): ReactNode {
  const entry = REGISTRY.get(card.type);
  if (!entry) {
    warnOnce(`render:${card.type}`, `[cards] no registry entry for type "${card.type}"`);
    return null;
  }
  // The map's value type is widened; each Component's prop type was specific
  // when registered, but at the call site we only know `WaveCardData`.
  // The discriminator (`card.type === entry.type`) guarantees runtime
  // alignment with the entry's Component prop type. createElement (not JSX)
  // so this file stays a plain .ts module — keeps the design-doc filename.
  return createElement(entry.Component as FC<CardComponentProps>, {
    card,
    onClose: opts.onClose,
  });
}

export function sizeFor(card: WaveCardData): CardSize {
  const entry = REGISTRY.get(card.type);
  if (!entry) {
    warnOnce(`size:${card.type}`, `[cards] no registry entry for type "${card.type}" — using fallback size`);
    return FALLBACK_SIZE;
  }
  return entry.defaultSize;
}

export interface AddPanelMenuItem {
  type: string;
  label: string;
  /** Optional create-form schema. The menu host shows the inline config
   *  card if this is set; otherwise the kind is created immediately. */
  createSchema?: CreateSchema;
}

/** Entries that opted into the AddPanel menu. */
export function addPanelEntries(): AddPanelMenuItem[] {
  const out: AddPanelMenuItem[] = [];
  for (const entry of REGISTRY.values()) {
    if (entry.addPanel) {
      out.push({
        type: String(entry.type),
        label: entry.addPanel.label,
        createSchema: entry.addPanel.createSchema,
      });
    }
  }
  return out;
}

/** Kernel-card → UI-card adapter. Iterates registry entries with a
 *  `fromKernel` adapter and returns the first non-null match.
 *
 *  Plugin cards (kind starts with `ui://`) are caught by
 *  `PluginIframeEntry.fromKernel`, which emits the `'plugin'` discriminator.
 *  Only `ui://` is accepted; the legacy `plugin:` form was deleted in M4.
 *  The actual AppBridge mount + tool call wiring is the M5 full-integration
 *  concern.
 */
export function adaptKernelCard(k: KernelCard): WaveCardData | null {
  for (const entry of REGISTRY.values()) {
    if (!entry.fromKernel) continue;
    const adapted = entry.fromKernel(k);
    if (adapted) return adapted;
  }
  return null;
}
