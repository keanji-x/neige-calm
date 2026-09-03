// The single module in the tree that turns reducer effects into TanStack cache
// operations. `core/events` deliberately never imports a QueryClient: the pure
// layer plans *what* is stale, this adapter decides *how* that becomes a cache
// call, and `event-bridge.tsx` owns the stream lifecycle around it.
//
// Every key the pure plan emits is a legacy key shape. This module is the one
// place that translates those shapes into `queryKeys` from app/providers; it
// must never invent a key shape of its own, and a plan key with no query behind
// it is dropped here rather than turned into a fabricated key.

import { toArea } from '../../../../core/domain/area.ts';
import type { QueryKey } from '../../../../core/events/invalidation-plan.ts';
import type { EventEffect } from '../../../../core/events/reducer.ts';
import { queryKeys } from '../providers/queries.ts';

/**
 * The slice of `QueryClient` the adapter is allowed to use. Narrowing it keeps
 * the module unit-testable with a recording fake and makes the blast radius of
 * an event frame readable.
 */
export interface QueryCachePort {
  invalidateQueries(filters?: { queryKey?: readonly unknown[] }): unknown;
  removeQueries(filters: { queryKey: readonly unknown[] }): unknown;
  getQueryData<T>(queryKey: readonly unknown[]): T | undefined;
  setQueryData<T>(queryKey: readonly unknown[], value: T): unknown;
  clear(): void;
}

/**
 * Translates one planned key onto a `queryKeys` key, or `null` when the built
 * surface has no query for it. See README.md for the per-kind table and the
 * reason behind each drop.
 */
export function mapPlannedQueryKey(key: QueryKey): readonly unknown[] | null {
  const [head, first, second] = key;
  if (head === 'areas' && key.length === 1) return queryKeys.areas();
  if (head === 'tracks' && first === 'area' && typeof second === 'string') return queryKeys.tracksInArea(second);
  if (head === 'track' && typeof first === 'string' && key.length === 2) return queryKeys.trackDetail(first);
  if (head === 'overlays' && (first === 'track' || first === 'card')) return queryKeys.overlaysByKind(first);
  if (head === 'harness-items' && typeof first === 'string' && key.length === 2) return queryKeys.harnessItems(first);
  if (head === 'spec-run' && typeof first === 'string' && key.length === 2) return queryKeys.specRun(first);
  /* The track's task verdicts. Both arities are mapped, and the bare one is not
     an oversight: the four `task.*` events carry no track-id *field*, so
     `derivedTrackId` cannot name one and the plan emits the prefix. (Their
     `idempotency_key` is the task id, which embeds the track id — the plan
     declines to parse it; see `queryKeys.trackReportPrefix` for why.) Dropping
     the prefix would leave the TASKS panel dead for exactly the events that
     change it. */
  if (head === 'track-report' && key.length === 1) return queryKeys.trackReportPrefix();
  if (head === 'track-report' && typeof first === 'string' && key.length === 2) return queryKeys.trackReport(first);
  /* The area drawer's conversation list. Only the bare form exists: no
     conversation-writing event carries a `area_id` and no cached row can supply
     one, so the plan emits the prefix and `queryKeys.areaConversations` keeps
     the area id in second position precisely so this prefix reaches it. */
  if (head === 'area-conversations' && key.length === 1) return queryKeys.areaConversationsPrefix();
  /* One track's conversation list. Both arities are mapped, same as
     `track-report` above: the plan names the track whenever `derivedTrackId`
     resolves one and falls back to the prefix when a `runtime.*` event's card
     belongs to a track no cached detail owns.

     The query behind these keys arrives in S5. Mapping them now is harmless —
     invalidating a key with no mounted query neither marks nor refetches
     anything — and mapping them *later* is what would be dangerous: a mounted
     query with no adapter arm is a list that silently never refreshes. */
  /* #1253 §6 — the Today resolve. One entry with no id: the kernel's partial
     unique index makes `purpose = 'launchpad'` a singleton, and the id is what
     that query is fetching. Without this arm `track.report_edited`'s
     `['today-launchpad']` is dropped right here and the page never learns the
     report stopped being empty. */
  if (head === 'today-launchpad' && key.length === 1) return queryKeys.todayLaunchpad();
  if (head === 'track-conversations' && key.length === 1) return queryKeys.trackConversationsPrefix();
  if (head === 'track-conversations' && typeof first === 'string' && key.length === 2) {
    return queryKeys.trackConversations(first);
  }
  return null;
}

/**
 * Applies the effects of one reduction. `persist-cursor` and `reconnect` are
 * stream lifecycle, not cache work, so the bridge handles them. Write-through
 * updates only an existing cached area; a missing row remains absent until the
 * accompanying invalidation refetches authoritative data.
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
    if (effect.type === 'write-through') {
      for (const write of effect.writes) {
        if (write.mode !== 'replace-existing-area') continue;
        const mapped = mapPlannedQueryKey(write.key);
        if (mapped === null) continue;
        const existing = client.getQueryData<readonly ReturnType<typeof toArea>[]>(mapped);
        if (existing === undefined || !existing.some((area) => area.id === write.value.id)) continue;
        const updated = toArea(write.value);
        client.setQueryData(mapped, existing.map((area) => area.id === updated.id ? updated : area));
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
