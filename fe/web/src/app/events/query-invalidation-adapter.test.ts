import { describe, expect, it } from 'vitest';

import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import type { EventEffect } from '../../../../core/events/reducer.ts';
import { queryKeys } from '../providers/queries.ts';
import { applyEventEffects, mapPlannedQueryKey, type QueryCachePort } from './query-invalidation-adapter.ts';

type Call = Readonly<{ op: 'invalidate' | 'remove' | 'clear'; queryKey?: readonly unknown[] }>;

function recordingClient() {
  const calls: Call[] = [];
  const client: QueryCachePort = {
    invalidateQueries: (filters?: { queryKey?: readonly unknown[] }) => {
      calls.push({ op: 'invalidate', queryKey: filters?.queryKey });
    },
    removeQueries: (filters: { queryKey: readonly unknown[] }) => {
      calls.push({ op: 'remove', queryKey: filters.queryKey });
    },
    clear: () => { calls.push({ op: 'clear' }); },
  };
  return { calls, client };
}

describe('query invalidation adapter', () => {
  it('maps every planned key shape onto a queryKeys key and drops the rest', () => {
    expect(mapPlannedQueryKey(['coves'])).toEqual(queryKeys.coves());
    expect(mapPlannedQueryKey(['waves', 'cove', 'c1'])).toEqual(queryKeys.wavesInCove('c1'));
    expect(mapPlannedQueryKey(['wave', 'w1'])).toEqual(queryKeys.waveDetail('w1'));
    expect(mapPlannedQueryKey(['overlays', 'wave'])).toEqual(queryKeys.overlaysByKind('wave'));
    expect(mapPlannedQueryKey(['overlays', 'card'])).toEqual(queryKeys.overlaysByKind('card'));
    for (const dropped of [['wave-files'], ['wave-files', 'w1'], ['waves-range'], ['wave-backlinks'], ['nope']]) {
      expect(mapPlannedQueryKey(dropped)).toBeNull();
    }
  });

  it('never emits a key the built surface does not define', () => {
    // The mapped cove-list key must be the very key the cove query registers,
    // otherwise an invalidation silently refreshes nothing.
    expect(mapPlannedQueryKey(['coves'])).toEqual(['coves']);
    expect(mapPlannedQueryKey(['waves', 'cove', 'c1'])).toEqual(['waves', 'c1']);
  });

  it('invalidates each mapped key of an invalidate effect', () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'invalidate', keys: [['coves'], ['wave', 'w1'], ['wave-files', 'w1']] }]);
    expect(calls).toEqual([
      { op: 'invalidate', queryKey: ['coves'] },
      { op: 'invalidate', queryKey: ['wave', 'w1'] },
    ]);
  });

  it('invalidates the whole cache for a null key set', () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'invalidate', keys: null }]);
    expect(calls).toEqual([{ op: 'invalidate', queryKey: undefined }]);
  });

  it('clears the cache for a clear-cache effect', () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'clear-cache' }]);
    expect(calls).toEqual([{ op: 'clear' }]);
  });

  it('removes mapped keys for a remove effect', () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'remove', keys: [['wave', 'w1'], ['waves-range']] }]);
    expect(calls).toEqual([{ op: 'remove', queryKey: ['wave', 'w1'] }]);
  });

  it('ignores lifecycle and write-through effects, which are not cache work', () => {
    const { calls, client } = recordingClient();
    const effects: EventEffect[] = [
      { type: 'persist-cursor', id: 7 },
      { type: 'reconnect' },
      { type: 'write-through', writes: [] },
    ];
    applyEventEffects(client, effects);
    expect(calls).toEqual([]);
  });

  it('preserves effect order across a snapshot-required style batch', () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [
      { type: 'clear-cache' },
      { type: 'invalidate', keys: [['coves']] },
      { type: 'remove', keys: [['wave', 'w1']] },
    ]);
    expect(calls.map((call) => call.op)).toEqual(['clear', 'invalidate', 'remove']);
  });

  it('turns a real cove.updated plan into a cove-list invalidation', () => {
    const cove = { id: 'c1', name: 'Cove', color: '#fff', sort: 0, kind: 'user', created_at: 1, updated_at: 2 } as const;
    const plan = invalidationPlanFor({ ev: 'cove.updated', data: cove });
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'invalidate', keys: plan.invalidate }]);
    expect(calls).toEqual([{ op: 'invalidate', queryKey: queryKeys.coves() }]);
  });

  it('turns a real wave.deleted plan into cove-list plus overlay invalidation and a detail removal', () => {
    const plan = invalidationPlanFor({
      ev: 'wave.deleted',
      data: { id: 'w1', cove_id: 'c1' },
    });
    const { calls, client } = recordingClient();
    applyEventEffects(client, [
      { type: 'invalidate', keys: plan.invalidate },
      { type: 'remove', keys: plan.remove },
    ]);
    expect(calls).toEqual([
      { op: 'invalidate', queryKey: queryKeys.wavesInCove('c1') },
      { op: 'invalidate', queryKey: queryKeys.overlaysByKind('wave') },
      { op: 'remove', queryKey: queryKeys.waveDetail('w1') },
    ]);
  });
});
