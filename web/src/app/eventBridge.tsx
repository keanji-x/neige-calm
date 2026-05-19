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

export function EventBridge() {
  const queryClient = useQueryClient();

  useEffect(() => {
    const stream = sharedEventStream();
    stream.subscribe(['*']);
    const off = stream.on((ev) => dispatch(queryClient, ev));
    return () => off();
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
