// Kernel-wire → UI-shape adapters.
//
// The kernel deliberately stores only structural facts (Cove/Wave/Card).
// Status, progress, ETA — everything semantic — comes from plugin
// overlays. Until the plugin host lands (M3), we fall back to sane
// "no plugin" defaults so the UI still has something to render.

import type {
  Cove,
  FsmCounts,
  FsmState,
  Wave,
  WaveCardData,
  WaveLifecycle,
  WaveStatus,
} from '../types';
import type {
  KernelCard,
  KernelCove,
  KernelOverlay,
  KernelWave,
} from './wire';
import { adaptKernelCard } from '../cards/registry';

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
  const running = waves.filter((w) => w.status === 'running').length;
  const noun = waves.length === 1 ? 'wave' : 'waves';
  if (running === 0) return `${waves.length} ${noun}`;
  return `${waves.length} ${noun} · ${running} running`;
}

/**
 * Folds the wave's own overlays into the UI shape. Recognized overlay kinds:
 *   - `"status"`   payload: `{ state: "running" | "waiting" }` (legacy plugin
 *                  overlays) OR `{ state: <FsmState>, counts: { working,
 *                  awaiting, errored } }` (kernel-owned `card_fsm` rows).
 *                  The latter is projected down to the 3-state legacy enum
 *                  for `Wave.status` while the full FSM string + counts ride
 *                  along on `Wave.fsmState` / `Wave.counts` for the new
 *                  dot/badge UI.
 *   - `"progress"` payload: `{ value: number }`  (0..1)
 *   - `"eta"`      payload: `{ text: string }`
 *   - `"now"`      payload: `{ text: string }`
 *
 * Anything else is ignored. Multiple plugins setting the same kind is
 * last-write-wins by overlay order — once a real plugin model exists we'll
 * pick by `plugin_id` priority.
 */
export function adaptWave(k: KernelWave, overlays: KernelOverlay[] = []): Wave {
  // Defaults: idle, no progress, no eta/now text. The UI hides empty
  // strings rather than rendering placeholder dashes, so a wave with no
  // overlays reads as a quiet structural row, not a half-broken status pill.
  let status: WaveStatus = 'idle';
  let fsmState: FsmState | undefined;
  let counts: FsmCounts | undefined;
  let progress = 0;
  let eta = '';
  let now = '';

  for (const o of overlays) {
    if (o.entity_kind !== 'wave' || o.entity_id !== k.id) continue;
    const p = o.payload as Record<string, unknown> | null;
    if (!p) continue;
    if (o.kind === 'status' && typeof p.state === 'string') {
      // Two shapes coexist:
      //   1. Legacy plugin overlays:  { state: "running" | "waiting" }
      //   2. kernel card_fsm overlays: { state: FsmState, counts: {...} }
      // We map both into the 3-state legacy `status` field, and additionally
      // surface the raw FSM state + counts when present (kernel-owned rows
      // are recognizable by their PascalCase state names).
      const stateStr = p.state;
      if (stateStr === 'running') status = 'running';
      else if (stateStr === 'waiting') status = 'waiting';
      else if (isFsmState(stateStr)) {
        fsmState = stateStr;
        status = projectFsmToLegacy(stateStr);
      }
      const c = p.counts;
      if (
        c &&
        typeof c === 'object' &&
        typeof (c as Record<string, unknown>).working === 'number' &&
        typeof (c as Record<string, unknown>).awaiting === 'number' &&
        typeof (c as Record<string, unknown>).errored === 'number'
      ) {
        const cc = c as Record<string, number>;
        counts = {
          working: cc.working,
          awaiting: cc.awaiting,
          errored: cc.errored,
        };
      }
    } else if (o.kind === 'progress' && typeof p.value === 'number') {
      progress = p.value;
    } else if (o.kind === 'eta' && typeof p.text === 'string') {
      eta = p.text;
    } else if (o.kind === 'now' && typeof p.text === 'string') {
      now = p.text;
    }
  }

  return {
    id: k.id,
    coveId: k.cove_id,
    title: k.title,
    status,
    // Issue #145 — the kernel always stamps a lifecycle on wave rows
    // (defaults to 'draft' on create, advanced explicitly by the Spec
    // Agent). Wire payloads from pre-#145 servers may omit the field;
    // mirror the zod schema's default and fall back to 'draft'.
    lifecycle: (k.lifecycle as WaveLifecycle | undefined) ?? 'draft',
    fsmState,
    counts,
    progress,
    eta,
    now,
  };
}

function isFsmState(s: string): s is FsmState {
  return (
    s === 'Starting' ||
    s === 'Idle' ||
    s === 'Working' ||
    s === 'AwaitingInput' ||
    s === 'Errored' ||
    s === 'Done'
  );
}

/**
 * Map the 6-state FSM down to the legacy 3-state `WaveStatus` so existing
 * groupers (Sidebar's "Waiting on you", Today's running count, Cove's
 * idle/waiting/running buckets) keep working without per-callsite changes.
 *
 *   AwaitingInput → waiting (block-on-user)
 *   Errored       → waiting (needs attention; same visual prominence)
 *   Working       → running
 *   Starting      → running (pre-tool-use phase is a flavor of activity)
 *   Idle | Done   → idle    (calm)
 */
function projectFsmToLegacy(s: FsmState): WaveStatus {
  switch (s) {
    case 'AwaitingInput':
    case 'Errored':
      return 'waiting';
    case 'Working':
    case 'Starting':
      return 'running';
    case 'Idle':
    case 'Done':
      return 'idle';
  }
}

/**
 * Map a kernel Card to one of the UI's card variants. Returns `null` for
 * unrecognized kinds so the caller can skip the row entirely.
 *
 * Per-kind adaptation lives on each `CardEntry.fromKernel` in
 * `cards/builtins/*.tsx`; the registry iterates them and returns the first
 * non-null match. Plugin cards accept only the canonical `ui://<plugin>/<view>`
 * URI — M4's hard cut deleted the legacy `plugin:<id>:<view>` parser; the
 * only consumer (hello-world) is rewritten in M6.
 */
export function adaptCard(k: KernelCard): WaveCardData | null {
  return adaptKernelCard(k);
}
