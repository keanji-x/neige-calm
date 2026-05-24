// Kernel-wire → UI-shape adapters.
//
// The kernel deliberately stores only structural facts (Cove/Wave/Card).
// Status, progress, ETA — everything semantic — comes from plugin
// overlays. Until the plugin host lands (M3), we fall back to sane
// "no plugin" defaults so the UI still has something to render.

import type { Cove, Wave, WaveCardData, WaveLifecycle } from '../types';
import type {
  KernelCard,
  KernelCove,
  KernelOverlay,
  KernelWave,
} from './wire';
import { adaptKernelCard } from '../cards/registry';
import { isRunning } from '../shared/lifecycle';

/**
 * Adapt a kernel Cove to the UI shape.
 *
 * The mockup carried a `subtitle` ("Personal site", "Client · e-commerce"),
 * which the kernel does not store. Rather than adding a column for it, we
 * leave the field present but blank — the page can derive a secondary
 * (wave count, running count) when it wants something in the eyebrow.
 */
export function adaptCove(k: KernelCove): Cove {
  return { id: k.id, name: k.name, subtitle: '', color: k.color };
}

/**
 * Derive a small text summary suitable for a cove's secondary line, e.g.
 * `"3 waves · 1 running"`. Returns an empty string for an empty cove so
 * the renderer can drop the line entirely.
 */
export function coveSummary(waves: Wave[]): string {
  if (waves.length === 0) return '';
  const running = waves.filter((w) => isRunning(w.lifecycle)).length;
  const noun = waves.length === 1 ? 'wave' : 'waves';
  if (running === 0) return `${waves.length} ${noun}`;
  return `${waves.length} ${noun} · ${running} running`;
}

/**
 * Folds the wave's own overlays into the UI shape. Recognized overlay kinds:
 *   - `"progress"` payload: `{ value: number }`  (0..1)
 *   - `"eta"`      payload: `{ text: string }`
 *   - `"now"`      payload: `{ text: string }`
 *   - `"any_card_needs_input"` payload: `{ value: boolean }` (issue #254 —
 *     written by the kernel `card_fsm` projector; OR'd with lifecycle at
 *     `shared/lifecycle.ts::waveNeedsUserAttention` for the sidebar
 *     "Waiting on you" grouping).
 *
 * Anything else is ignored. Wave-level lifecycle lives on the
 * `WaveLifecycle` field stamped on the kernel `Wave` row — not on
 * overlays — so this adapter does NOT read `kind:"status"` for waves.
 * The per-card FSM still writes card-scoped status overlays, which the
 * codex card head consumes directly.
 *
 * Multiple plugins setting the same kind is last-write-wins by overlay
 * order — once a real plugin model exists we'll pick by `plugin_id`
 * priority.
 */
export function adaptWave(k: KernelWave, overlays: KernelOverlay[] = []): Wave {
  let progress = 0;
  let eta = '';
  let now = '';
  let anyCardNeedsInput = false;

  for (const o of overlays) {
    if (o.entity_kind !== 'wave' || o.entity_id !== k.id) continue;
    const p = o.payload as Record<string, unknown> | null;
    if (!p) continue;
    if (o.kind === 'progress' && typeof p.value === 'number') {
      progress = p.value;
    } else if (o.kind === 'eta' && typeof p.text === 'string') {
      eta = p.text;
    } else if (o.kind === 'now' && typeof p.text === 'string') {
      now = p.text;
    } else if (o.kind === 'any_card_needs_input' && typeof p.value === 'boolean') {
      anyCardNeedsInput = p.value;
    }
  }

  return {
    id: k.id,
    coveId: k.cove_id,
    title: k.title,
    // Issue #145 — the kernel always stamps a lifecycle on wave rows
    // (defaults to 'draft' on create, advanced explicitly by the Spec
    // Agent). Wire payloads from pre-#145 servers may omit the field;
    // mirror the zod schema's default and fall back to 'draft'.
    lifecycle: (k.lifecycle as WaveLifecycle | undefined) ?? 'draft',
    anyCardNeedsInput,
    progress,
    eta,
    now,
    // Issue #250 PR 5 — preserve the kernel timestamps so the Today
    // calendar rail can derive "active on day D" without a second
    // fetch. `terminal_at` is nullable on the wire (open waves);
    // normalize `undefined` from optional zod fields to `null` so the
    // UI side never has to distinguish.
    createdAt: k.created_at,
    terminalAt: k.terminal_at ?? null,
  };
}

/**
 * Map a kernel Card to one of the UI's card variants. Returns `null` for
 * unrecognized kinds so the caller can skip the row entirely.
 *
 * Per-kind adaptation lives on each `CardEntry.fromKernel` in
 * `cards/builtins/*.tsx`; the registry iterates them and returns the first
 * non-null match. Plugin cards accept only the canonical `ui://<plugin>/<view>`
 * URI — M4's hard cut deleted the legacy `plugin:<id>:<view>` parser; the
 * hello-world demo (its last consumer) was deleted alongside the
 * WaveLifecycle unification.
 */
export function adaptCard(k: KernelCard): WaveCardData | null {
  return adaptKernelCard(k);
}
