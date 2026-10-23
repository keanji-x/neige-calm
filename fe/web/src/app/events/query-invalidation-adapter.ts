// The single module that turns reducer effects into TanStack cache operations. It translates the
// plan's legacy key shapes into `queryKeys`; a plan key with no query behind it is dropped, never fabricated.

import { newestArea, toArea } from '../../../../core/domain/area.ts';
import type { QueryKey } from '../../../../core/events/invalidation-plan.ts';
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
function invalidateMappedQuery(client: QueryCachePort, queryKey: readonly unknown[]): void {
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

/** Translates one planned key onto a `queryKeys` key, or `null` when the built surface has no query for it. */
export function mapPlannedQueryKey(key: QueryKey): readonly unknown[] | null {
  const [head, first, second] = key;
  if (head === 'model-catalog' && key.length === 2 && typeof first === 'object' && first !== null
    && 'kind' in first && first.kind === 'card' && 'cardId' in first && typeof first.cardId === 'string') {
    return queryKeys.modelCatalog({ kind: 'card', cardId: first.cardId });
  }
  if (head === 'track-backlinks' && key.length === 1) return queryKeys.trackBacklinksPrefix();
  if (head === 'plugins' && key.length === 1) return queryKeys.plugins();
  if (head === 'track-templates' && key.length === 1) return queryKeys.trackTemplates();
  if (head === 'plugin-data' && typeof first === 'string' && key.length === 2) return queryKeys.pluginDataPrefix(first);
  if (head === 'areas' && key.length === 1) return queryKeys.areas();
  if (head === 'tracks' && first === 'area' && typeof second === 'string') return queryKeys.tracksInArea(second);
  if (head === 'track' && typeof first === 'string' && key.length === 2) return queryKeys.trackDetail(first);
  if (head === 'overlays' && (first === 'track' || first === 'card')) return queryKeys.overlaysByKind(first);
  if (head === 'harness-items' && typeof first === 'string' && key.length === 2) return queryKeys.harnessItems(first);
  if (head === 'planner-run' && typeof first === 'string' && key.length === 2) return queryKeys.plannerRun(first);
  if (head === 'conversation-initial-input' && key.length === 1) return queryKeys.initialInputPrefix();
  if (head === 'conversation-initial-input' && typeof first === 'string' && key.length === 2) return queryKeys.initialInputTrack(first);
  if (head === 'conversation-initial-input' && typeof first === 'string' && typeof second === 'string' && key.length === 3) {
    return queryKeys.initialInput(first, second);
  }
  /* Both arities: the `task.*` events carry no track-id field (the plan declines to parse the task id),
       so dropping the prefix would leave the TASKS panel dead for exactly the events that change it. */
  if (head === 'track-report' && key.length === 1) return queryKeys.trackReportPrefix();
  if (head === 'track-report' && typeof first === 'string' && key.length === 2) return queryKeys.trackReport(first);
  /* One entry with no id: `purpose = 'launchpad'` is a singleton on the kernel side. */
  if (head === 'today-launchpad' && key.length === 1) return queryKeys.todayLaunchpad();
  if (head === 'track-conversations' && key.length === 1) return queryKeys.trackConversationsPrefix();
  if (head === 'track-conversations' && typeof first === 'string' && key.length === 2) {
    return queryKeys.trackConversations(first);
  }
  return null;
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
        const mapped = mapPlannedQueryKey(key);
        if (mapped !== null) invalidateMappedQuery(client, mapped);
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
          const key = mapPlannedQueryKey(write.key);
          if (key !== null) void client.resetQueries({ queryKey: key });
          continue;
        }
        const mapped = mapPlannedQueryKey(write.key);
        if (mapped === null) continue;
        const existing = client.getQueryData<readonly ReturnType<typeof toArea>[]>(mapped);
        if (existing === undefined || !existing.some((area) => area.id === write.value.id)) continue;
        const updated = toArea(write.value);
        client.setQueryData(mapped, existing.map((area) => area.id === updated.id
          ? newestArea(area, updated)
          : area));
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
