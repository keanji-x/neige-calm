// The single module in the tree that turns reducer effects into TanStack cache
// operations. `core/events` deliberately never imports a QueryClient: the pure
// layer plans *what* is stale, this adapter decides *how* that becomes a cache
// call, and `event-bridge.tsx` owns the stream lifecycle around it.
//
// Every key the pure plan emits is a legacy key shape. This module is the one
// place that translates those shapes into `queryKeys` from app/providers; it
// must never invent a key shape of its own, and a plan key with no query behind
// it is dropped here rather than turned into a fabricated key.

import type { QueryKey } from '../../../../core/events/invalidation-plan.ts';
import type { EventEffect } from '../../../../core/events/reducer.ts';
import { queryKeys } from '../providers/queries.ts';

/**
 * The slice of `QueryClient` the adapter is allowed to use. Narrowing it keeps
 * the module unit-testable with a recording fake and makes the blast radius of
 * an event frame readable: invalidate, remove, clear — nothing else.
 */
export interface QueryCachePort {
  invalidateQueries(filters?: { queryKey?: readonly unknown[] }): unknown;
  removeQueries(filters: { queryKey: readonly unknown[] }): unknown;
  clear(): void;
}

/**
 * Translates one planned key onto a `queryKeys` key, or `null` when the built
 * surface has no query for it. See README.md for the per-kind table and the
 * reason behind each drop.
 */
export function mapPlannedQueryKey(key: QueryKey): readonly unknown[] | null {
  const [head, first, second] = key;
  if (head === 'coves' && key.length === 1) return queryKeys.coves();
  if (head === 'waves' && first === 'cove' && typeof second === 'string') return queryKeys.wavesInCove(second);
  if (head === 'wave' && typeof first === 'string' && key.length === 2) return queryKeys.waveDetail(first);
  if (head === 'overlays' && (first === 'wave' || first === 'card')) return queryKeys.overlaysByKind(first);
  return null;
}

/**
 * Applies the effects of one reduction. `persist-cursor` and `reconnect` are
 * stream lifecycle, not cache work, so the bridge handles them; `write-through`
 * is deliberately ignored (README: the event payload is a wire cove, while the
 * cove list caches decoded domain coves, and the same event already invalidates
 * `['coves']`).
 */
export function applyEventEffects(client: QueryCachePort, effects: readonly EventEffect[]): void {
  for (const effect of effects) {
    if (effect.type === 'clear-cache') {
      client.clear();
      continue;
    }
    if (effect.type === 'invalidate') {
      // A null key set is the reducer's "everything is suspect" signal after a
      // replay: invalidate the whole cache rather than guessing a key list.
      if (effect.keys === null) {
        void client.invalidateQueries();
        continue;
      }
      for (const key of effect.keys) {
        const mapped = mapPlannedQueryKey(key);
        if (mapped !== null) void client.invalidateQueries({ queryKey: mapped });
      }
      continue;
    }
    if (effect.type === 'remove') {
      for (const key of effect.keys) {
        const mapped = mapPlannedQueryKey(key);
        if (mapped !== null) void client.removeQueries({ queryKey: mapped });
      }
    }
  }
}
