// The single module that turns reducer effects into TanStack cache operations. It translates the
// plan's legacy key shapes into `queryKeys`; a plan key with no query behind it is dropped, never fabricated.

import { newestArea, toArea } from '../../../../core/domain/area.ts';
import type { QueryKey } from '../../../../core/events/invalidation-plan.ts';
import type { EventEffect } from '../../../../core/events/reducer.ts';
import { queryKeys } from '../providers/queries.ts';

/** The slice of `QueryClient` the adapter is allowed to use. */
export interface QueryCachePort {
  cancelQueries(filters: { queryKey: readonly unknown[] }): Promise<unknown>;
  invalidateQueries(filters?: { queryKey?: readonly unknown[] }): unknown;
  removeQueries(filters: { queryKey: readonly unknown[] }): unknown;
  getQueryData<T>(queryKey: readonly unknown[]): T | undefined;
  setQueryData<T>(queryKey: readonly unknown[], value: T): unknown;
  clear(): void;
}

/**
 * TanStack reuses an in-flight initial fetch even after `invalidateQueries`, and the stale response
 * then clears `isInvalidated`; task verdicts do not all poll, so cancel the report fetch first.
 */
function invalidateMappedQuery(client: QueryCachePort, queryKey: readonly unknown[]): void {
  if (queryKey[0] !== 'track-report') {
    void client.invalidateQueries({ queryKey });
    return;
  }
  void client.cancelQueries({ queryKey }).then(() => client.invalidateQueries({ queryKey }));
}

/** Translates one planned key onto a `queryKeys` key, or `null` when the built surface has no query for it. */
export function mapPlannedQueryKey(key: QueryKey): readonly unknown[] | null {
  const [head, first, second] = key;
  if (head === 'areas' && key.length === 1) return queryKeys.areas();
  if (head === 'tracks' && first === 'area' && typeof second === 'string') return queryKeys.tracksInArea(second);
  if (head === 'track' && typeof first === 'string' && key.length === 2) return queryKeys.trackDetail(first);
  if (head === 'overlays' && (first === 'track' || first === 'card')) return queryKeys.overlaysByKind(first);
  if (head === 'harness-items' && typeof first === 'string' && key.length === 2) return queryKeys.harnessItems(first);
  if (head === 'planner-run' && typeof first === 'string' && key.length === 2) return queryKeys.plannerRun(first);
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
        void client.invalidateQueries();
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
        if (write.mode !== 'replace-existing-area') continue;
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
