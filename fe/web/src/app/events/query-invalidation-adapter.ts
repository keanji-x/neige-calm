// Turns reducer effects into TanStack cache operations using the shared resource addresses.

import { newestArea, toArea } from '../../../../core/domain/area.ts';
import type { EventEffect } from '../../../../core/events/reducer.ts';
import { queryKeys } from '../providers/query-keys.ts';
import { retireAreaQueries, type AreaQueryLifecyclePort } from '../providers/area-query-lifecycle.ts';
import { cancelThenInvalidate } from './query-refresh.ts';

/** The slice of `QueryClient` the adapter is allowed to use. */
export interface QueryCachePort extends AreaQueryLifecyclePort {
  invalidateQueries(filters?: { queryKey?: readonly unknown[] }): unknown;
  resetQueries(filters: { queryKey: readonly unknown[] }): unknown;
  removeQueries(filters: { queryKey: readonly unknown[] }): unknown;
  getQueryData<T>(queryKey: readonly unknown[]): T | undefined;
  clear(): void;
}

/**
 * TanStack reuses an in-flight initial fetch even after `invalidateQueries`, and the stale response
 * then clears `isInvalidated`. Event-driven reads need a replacement
 * read started after the event, even when the transport ignores abort.
 */
function invalidateQuery(client: QueryCachePort, queryKey: readonly unknown[]): void {
  const cancelFirst = queryKey[0] === 'track' || queryKey[0] === 'track-conversations'
    || queryKey[0] === 'track-report' || queryKey[0] === 'conversation-initial-input'
    || queryKey[0] === 'harness-items' || queryKey[0] === 'planner-run'
    || queryKey[0] === 'track-backlinks' || queryKey[0] === 'plugins'
    || queryKey[0] === 'track-templates' || queryKey[0] === 'plugin-data'
    || (queryKey[0] === 'overlays' && queryKey[1] === 'track');
  if (!cancelFirst) {
    void client.invalidateQueries({ queryKey });
    return;
  }
  void cancelThenInvalidate(client, queryKey);
}

/**
 * Applies the effects of one reduction; stream lifecycle effects are the bridge's. Write-through
 * updates only an existing cached area; a missing row stays absent until the invalidation refetches.
 */
export function applyEventEffects(client: QueryCachePort, effects: readonly EventEffect[]): void {
  for (const effect of effects) {
    if (effect.type === 'clear-cache') {
      client.clear();
      continue;
    }
    if (effect.type === 'invalidate') {
      // A null key set is the reducer's "everything is suspect" signal after a replay.
      if (effect.keys === null) {
        // A replay boundary cannot certify retained windows behind the latest page.
        void client.resetQueries({ queryKey: queryKeys.transcriptPrefix() });
        void cancelThenInvalidate(client, queryKeys.overlaysByKind('track'), null);
        continue;
      }
      for (const key of effect.keys) {
        invalidateQuery(client, key);
      }
      continue;
    }
    if (effect.type === 'write-through') {
      for (const write of effect.writes) {
        if (write.mode === 'area-deleted') {
          retireAreaQueries(client, write.areaId);
          continue;
        }
        if (write.mode === 'reset-query') {
          void client.resetQueries({ queryKey: write.key });
          continue;
        }
        const existing = client.getQueryData<readonly ReturnType<typeof toArea>[]>(write.key);
        if (existing === undefined || !existing.some((area) => area.id === write.value.id)) continue;
        const updated = toArea(write.value);
        client.setQueryData(write.key, existing.map((area) => area.id === updated.id
          ? newestArea(area, updated)
          : area));
      }
      continue;
    }
    if (effect.type === 'remove') {
      for (const key of effect.keys) {
        void client.removeQueries({ queryKey: key });
      }
    }
  }
}
