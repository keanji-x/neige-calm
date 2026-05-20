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
import { sharedEventStream } from '../api/events';
import { queryKeys } from '../api/queries';
import type { KernelWaveDetail, WireEvent } from '../api/wire';

/** Debounce window for card-event invalidations, keyed by wave_id. Tuned
 *  to comfortably swallow the ~10-20ms gap between the kernel's
 *  card.added and card.updated emissions during multi-step card creation
 *  (see header comment). Short enough that external clients still see a
 *  near-instant refresh. */
const CARD_INVALIDATE_DEBOUNCE_MS = 60;

export function EventBridge() {
  const queryClient = useQueryClient();

  useEffect(() => {
    const stream = sharedEventStream();
    stream.subscribe(['*']);

    // Per-wave timer so bursts on different waves don't suppress each other.
    const pendingCardInvalidations = new Map<string, ReturnType<typeof setTimeout>>();

    const off = stream.on((ev) => dispatch(queryClient, ev, pendingCardInvalidations));

    return () => {
      off();
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
  if (existing) clearTimeout(existing);
  const timer = setTimeout(() => {
    pending.delete(wave_id);
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
