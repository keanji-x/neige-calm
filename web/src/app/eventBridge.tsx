// EventBridge — wires the WS event stream to the Query cache.
//
// Mounted once inside <AppProviders>, this subscribes to the shared event
// stream and translates each kernel push into a `queryClient.invalidate
// Queries` call. Components don't need to subscribe to the bus themselves
// any more; they just re-render when their query re-fetches.
//
// Mapping (kept in sync with `api/schemas.ts` event variants):
//   cove.updated / cove.deleted   → invalidate ['coves']
//   wave.updated                  → invalidate ['waves', cove_id] + ['wave', id]
//   wave.deleted                  → invalidate ['waves', cove_id], drop ['wave', id]
//   card.added / .updated         → invalidate ['wave', wave_id]
//   card.deleted                  → invalidate ['wave', wave_id]
//   overlay.set / .deleted        → invalidate the affected wave detail AND
//                                    the global ['overlays', entity_kind]
//                                    snapshot used by the Sidebar
//   plugin.state                  → no-op (no plugin list query yet)
//
// Card events used to be debounced + suppressed during a multi-step
// `addCardOfKind` (POST card → POST terminal → PATCH payload), because the
// kernel emitted `card.added` with `payload=null` and then `card.updated`
// with the terminal_id within ~10ms — the UI would otherwise render the
// half-built card before snapping. #13 collapsed that flow into a single
// atomic endpoint that emits one `card.added` carrying the final payload,
// so the debounce + suppression scaffolding is gone and card events now
// invalidate immediately. TanStack Query coalesces any rapid follow-ons
// (e.g. the codex flow's `card.added` + `card.updated` pair).
//
// `overlay.{set,deleted}` is the only mildly tricky case: the kernel
// addresses overlays by `entity_kind` + `entity_id`, so for card overlays
// we don't know which wave to invalidate from the event payload alone. We
// inspect already-cached wave details to find the one that owns the card,
// matching the strategy `useKernel` used pre-migration. If no wave is
// loaded, the overlay just sits in the kernel until a wave detail refetch
// picks it up — the user can't see a card overlay change for a wave
// they're not on, so this is harmless.

import { useEffect } from 'react';
import { useQueryClient, type QueryClient } from '@tanstack/react-query';
import { sharedEventStream, type EventMeta } from '../api/events';
import { queryKeys } from '../api/queries';
import { dlog } from '../util/debug';
import type { KernelWaveDetail, WireEvent } from '../api/wire';

// ---------------------------------------------------------------------------
// Event trace exposure (issue #56 slice 5).
//
// When the app boots under a dev build with `?trace=1` in the URL, we mirror
// every event the WS layer surfaces into a window-scoped ring buffer. Playwright
// (and a curious dev poking DevTools) reads it via `window.__neigeEvents__` to
// assert "the event sequence that produced this UI state" alongside whatever
// ARIA / role state the test was already checking.
//
// Double-gated by `import.meta.env.DEV` AND the URL param so:
//   - production bundles tree-shake the buffer code (Vite folds `import.meta.env.DEV`
//     to a literal `false` at build time, so the whole branch becomes dead code);
//   - even in dev, browsing without `?trace=1` keeps `window.__neigeEvents__`
//     undefined — no accidental memory growth across long-running dev sessions.
//
// Ring-buffer cap is 200 events: comfortably more than any single Playwright
// scenario emits while still bounded enough that a forgotten dev tab can't
// balloon memory.
const TRACE_RING_CAP = 200;

/**
 * Shape captured into `window.__neigeEvents__` under the trace gate.
 * Exported so Playwright helpers can import the type and stay in sync
 * with what the bridge actually writes (no parallel duck-typed shape
 * floating around the e2e suite).
 */
export interface TraceEvent {
  id: number;
  eventVersion: number;
  ev: WireEvent['ev'];
  data: WireEvent['data'];
  ts: number;
}

// `Window` augmentation for the trace globals. Tiny, colocated with the
// only code that writes the buffer, so it doesn't pull a separate
// `types/` dir into the tree. Both fields are optional because they're
// only present under the dev + `?trace=1` gate.
declare global {
  interface Window {
    __neigeEvents__?: TraceEvent[];
    __neigeClearEvents__?: () => void;
  }
}

/** Read `?trace=1` (or any truthy value of the `trace` URL param). Tolerant of
 *  the SSR-ish "no window" path that vitest sometimes simulates — returns
 *  `false` rather than throwing. */
function traceFlagFromUrl(): boolean {
  if (typeof window === 'undefined' || typeof window.location === 'undefined') return false;
  try {
    return new URLSearchParams(window.location.search).has('trace');
  } catch {
    return false;
  }
}

/** Decide once per module load whether the ring buffer is active for this
 *  session. We don't reevaluate on navigation — the contract is "set ?trace=1
 *  on the initial page load" (matches how Playwright opens the app).
 *
 *  NOTE: callers should inline the `import.meta.env.DEV` short-circuit at the
 *  call site (not just rely on this fn returning false in prod) so Vite/terser
 *  can fold the entire trace branch — including the call to this function and
 *  to `ensureTraceBuffer` / `pushTraceEvent` — into dead code. See the
 *  `useEffect` body below for the canonical pattern. */
function isTraceEnabled(): boolean {
  return import.meta.env.DEV && traceFlagFromUrl();
}

/** Lazily install the buffer + clear function on `window` exactly once.
 *  Called from the EventBridge effect under the trace gate. Idempotent: a
 *  second call (e.g. hot-reload) reuses the existing buffer so test snapshots
 *  taken before the reload are still readable. */
function ensureTraceBuffer(): TraceEvent[] {
  if (!window.__neigeEvents__) {
    window.__neigeEvents__ = [];
  }
  if (!window.__neigeClearEvents__) {
    window.__neigeClearEvents__ = () => {
      const buf = window.__neigeEvents__;
      if (buf) buf.length = 0;
    };
  }
  return window.__neigeEvents__;
}

function pushTraceEvent(buf: TraceEvent[], ev: WireEvent, meta: EventMeta): void {
  buf.push({
    id: meta.id,
    eventVersion: meta.eventVersion,
    ev: ev.ev,
    data: ev.data,
    ts: Date.now(),
  });
  if (buf.length > TRACE_RING_CAP) {
    // Drop the oldest. `shift` is O(n) but n <= 201; a busy stream pays
    // a few microseconds per frame for the ergonomic ring shape.
    buf.shift();
  }
}

export function EventBridge() {
  const queryClient = useQueryClient();

  useEffect(() => {
    const stream = sharedEventStream();
    stream.subscribe(['*']);

    // Resolve the trace gate once per mount. We literally short-circuit
    // on `import.meta.env.DEV` HERE (not just inside `isTraceEnabled`)
    // so Vite/terser folds the whole right-hand side — including the
    // calls to `ensureTraceBuffer` / `pushTraceEvent` — into dead code
    // in production. Don't refactor this into a single fn call without
    // re-verifying with `grep __neigeEvents__ web/dist/assets/*.js`.
    const traceBuf: TraceEvent[] | null =
      import.meta.env.DEV && isTraceEnabled() ? ensureTraceBuffer() : null;

    const off = stream.on((ev, meta) => {
      dlog('eventBridge', 'RX', ev.ev, ev.data);
      if (traceBuf) pushTraceEvent(traceBuf, ev, meta);
      dispatch(queryClient, ev);
    });

    // Sync engine phase 2 (Scope D) — control-frame hooks.
    //
    // `_replay_complete` fires once after a reconnect's historical
    // window finishes streaming. Run a defensive batch invalidate so
    // any optimistic state that drifted during the disconnected window
    // converges. Cheap (TanStack batches the actual refetches), and
    // catches edge cases that the per-event dispatcher above would miss
    // (e.g. a card overlay whose wave detail isn't loaded right now).
    const offReplay = stream.onReplayComplete(() => {
      dlog('eventBridge', 'RX _replay_complete — running defensive batch invalidate');
      void queryClient.invalidateQueries();
    });

    // `_snapshot_required` fires when the server can't honor the cursor
    // (retention horizon). Clear the persisted React Query cache so the
    // next mount comes up cold and refetches from REST. The EventStream
    // has already wiped `lastEventId` by the time this listener runs.
    const offSnapshot = stream.onSnapshotRequired(() => {
      dlog('eventBridge', 'RX _snapshot_required — clearing query cache');
      queryClient.clear();
    });

    return () => {
      off();
      offReplay();
      offSnapshot();
    };
  }, [queryClient]);

  return null;
}

function dispatch(qc: QueryClient, ev: WireEvent): void {
  switch (ev.ev) {
    case 'cove.updated': {
      void qc.invalidateQueries({ queryKey: queryKeys.coves() });
      return;
    }
    case 'cove.deleted': {
      void qc.invalidateQueries({ queryKey: queryKeys.coves() });
      // Same orphan reasoning as wave.deleted — overlays attached to the
      // deleted cove's waves may not get individual cascade events.
      void qc.invalidateQueries({ queryKey: queryKeys.overlaysByKind('wave') });
      return;
    }
    case 'wave.updated': {
      const { id, cove_id } = ev.data;
      void qc.invalidateQueries({ queryKey: queryKeys.wavesInCove(cove_id) });
      void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(id) });
      return;
    }
    case 'wave.deleted': {
      const { id, cove_id } = ev.data;
      void qc.invalidateQueries({ queryKey: queryKeys.wavesInCove(cove_id) });
      qc.removeQueries({ queryKey: queryKeys.waveDetail(id) });
      // Kernel doesn't guarantee an overlay.deleted cascade per orphaned
      // overlay; refresh the global snapshot so stale entries vanish.
      void qc.invalidateQueries({ queryKey: queryKeys.overlaysByKind('wave') });
      return;
    }
    case 'card.added':
    case 'card.updated':
    case 'card.deleted': {
      // #13: the atomic terminal-card endpoint emits a single card.added
      // carrying the final payload, so invalidating immediately no longer
      // races a half-built intermediate state. TanStack Query coalesces
      // back-to-back invalidates (e.g. the codex flow's add+update pair)
      // into a single refetch.
      void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(ev.data.wave_id) });
      return;
    }
    case 'overlay.set':
    case 'overlay.deleted': {
      const ek = ev.data.entity_kind;
      const eid = ev.data.entity_id;
      if (ek === 'wave' || ek === 'card') {
        // Sidebar's status indicators read the global per-kind snapshot.
        void qc.invalidateQueries({ queryKey: queryKeys.overlaysByKind(ek) });
      }
      if (ek === 'wave') {
        void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(eid) });
      } else if (ek === 'card') {
        // Find any cached wave detail that owns this card and invalidate
        // it. Matches the pre-migration behavior of useKernel — if no
        // wave is loaded, the overlay change isn't visible yet anyway.
        const waveId = findWaveOwningCard(qc, eid);
        if (waveId) {
          void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(waveId) });
        }
      }
      return;
    }
    case 'plugin.state': {
      // No UI surface for plugin state yet. M3 will add a plugins query
      // here and invalidate it.
      return;
    }
    case 'codex.hook': {
      // Codex hooks don't change persisted state — the codex card subscribes
      // to its own card topic and consumes events directly. No query
      // invalidation required.
      return;
    }
    case 'codex.job_requested':
    case 'terminal.job_requested':
    case 'task.completed':
    case 'task.failed': {
      // PR4 of #136: kernel-internal dispatcher / task-lifecycle signals.
      // No UI invalidation — PR5's Dispatcher consumes via
      // EventBus::subscribe, and PR8's wait_for_events surfaces them to
      // the spec agent directly. The case arms exist so the discriminated
      // union is exhaustive and tsc catches a missed variant on the next
      // wire-shape change.
      return;
    }
  }
}

/** Search the loaded `['wave', *]` query data for a wave detail that
 *  contains a card with this id. Returns the first hit, or undefined. */
function findWaveOwningCard(qc: QueryClient, cardId: string): string | undefined {
  const entries = qc.getQueriesData<KernelWaveDetail>({ queryKey: ['wave'] });
  for (const [key, detail] of entries) {
    if (!detail) continue;
    if (detail.cards.some((c) => c.id === cardId)) {
      // key is ['wave', waveId]
      const waveId = key[1];
      if (typeof waveId === 'string') return waveId;
    }
  }
  return undefined;
}
