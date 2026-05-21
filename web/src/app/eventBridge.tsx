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
//   card.added / .updated         → debounced invalidate ['wave', wave_id]
//   card.deleted                  → debounced invalidate ['wave', wave_id]
//   overlay.set / .deleted        → invalidate the affected wave detail AND
//                                    the global ['overlays', entity_kind]
//                                    snapshot used by the Sidebar
//   plugin.state                  → no-op (no plugin list query yet)
//
// Why debounce card events: creating a terminal card today is a 3-step
// kernel mutation (POST card → POST terminal → PATCH payload), emitting
// `card.added` (no terminal_id yet) and `card.updated` (with terminal_id)
// within ~10ms. Without coalescing, the UI renders the half-built card
// first (static-text branch) and then swaps in `<XtermView>`, which the
// user sees as a visible twitch. A small wave-keyed debounce collapses
// rapid bursts into a single refetch carrying the final state. The fix
// belongs here (not in `addCardOfKind`) because any future multi-step
// kernel flow benefits automatically. A proper atomic create endpoint
// would obviate the workaround — tracked separately.
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

/** Debounce window for card-event invalidations, keyed by wave_id. Tuned
 *  to comfortably swallow the ~10-20ms gap between the kernel's
 *  card.added and card.updated emissions during multi-step card creation
 *  (see header comment). Short enough that external clients still see a
 *  near-instant refresh. */
const CARD_INVALIDATE_DEBOUNCE_MS = 60;

// ---------------------------------------------------------------------------
// Self-mutation suppression.
//
// While a client is in the middle of a multi-step kernel mutation (today:
// the 3-step terminal-card create), it should NOT react to the WS event
// echoes of its own intermediate writes — those echoes carry half-built
// state (e.g. card.added with payload=null before the terminal_id patch).
// The diagnostic logs that motivated this also showed the debounce window
// expiring before step 2 finished, so debouncing alone isn't sufficient.
//
// The originating mutation marks wave_ids as suppressed for its duration
// and fires its own invalidate at the end (the single, atomic-ish UI
// refresh). External clients (different sessions, plugins) don't see this
// flag and continue to handle events normally.
//
// When the atomic-create endpoint (#13) lands, the mutation collapses to
// a single API call emitting a single event with the final state; this
// suppression layer + addCardOfKind's try/finally can be removed wholesale.
const suppressionRefs = new Map<string, number>();

/** Mark `wave_id` as having an in-flight self-mutation; returns a release
 *  function that the caller MUST invoke (use try/finally) when done.
 *  Refcounted so concurrent mutations on the same wave nest safely. */
export function suppressCardEvents(wave_id: string): () => void {
  suppressionRefs.set(wave_id, (suppressionRefs.get(wave_id) ?? 0) + 1);
  return () => {
    const cur = suppressionRefs.get(wave_id) ?? 0;
    if (cur <= 1) suppressionRefs.delete(wave_id);
    else suppressionRefs.set(wave_id, cur - 1);
  };
}

function isWaveSuppressed(wave_id: string): boolean {
  return suppressionRefs.has(wave_id);
}

export function EventBridge() {
  const queryClient = useQueryClient();

  useEffect(() => {
    const stream = sharedEventStream();
    stream.subscribe(['*']);

    // Per-wave timer so bursts on different waves don't suppress each other.
    const pendingCardInvalidations = new Map<string, ReturnType<typeof setTimeout>>();

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
      dispatch(queryClient, ev, pendingCardInvalidations);
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
      for (const timer of pendingCardInvalidations.values()) clearTimeout(timer);
      pendingCardInvalidations.clear();
    };
  }, [queryClient]);

  return null;
}

function scheduleCardInvalidate(
  qc: QueryClient,
  wave_id: string,
  pending: Map<string, ReturnType<typeof setTimeout>>,
): void {
  const existing = pending.get(wave_id);
  if (existing) {
    clearTimeout(existing);
    dlog('eventBridge', 'card invalidate RESET timer', { wave_id });
  } else {
    dlog('eventBridge', 'card invalidate START timer', { wave_id });
  }
  const timer = setTimeout(() => {
    pending.delete(wave_id);
    dlog('eventBridge', 'card invalidate FIRE', { wave_id });
    void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(wave_id) });
  }, CARD_INVALIDATE_DEBOUNCE_MS);
  pending.set(wave_id, timer);
}

function dispatch(
  qc: QueryClient,
  ev: WireEvent,
  pendingCardInvalidations: Map<string, ReturnType<typeof setTimeout>>,
): void {
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
      if (isWaveSuppressed(ev.data.wave_id)) {
        dlog('eventBridge', 'card event SUPPRESSED (self-mutation in flight)', {
          ev: ev.ev,
          wave_id: ev.data.wave_id,
        });
        return;
      }
      scheduleCardInvalidate(qc, ev.data.wave_id, pendingCardInvalidations);
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
