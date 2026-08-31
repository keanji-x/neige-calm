import { describe, expect, expectTypeOf, it } from 'vitest';

import type { CoveWire } from '../../../../core/domain/cove.ts';
import { wireEventSchema, type WireEvent } from '../../../../core/api/schemas.ts';
import type { CacheWrite } from '../../../../core/events/invalidation-plan.ts';
import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import type { EventEffect } from '../../../../core/events/reducer.ts';
import { queryKeys } from '../providers/queries.ts';
import { applyEventEffects, mapPlannedQueryKey, type QueryCachePort } from './query-invalidation-adapter.ts';

type Call = Readonly<{ op: 'invalidate' | 'remove' | 'set' | 'clear'; queryKey?: readonly unknown[] }>;

function recordingClient(initialCoves?: readonly ReturnType<typeof import('../../../../core/domain/cove.ts').toCove>[]) {
  const calls: Call[] = [];
  let coves = initialCoves;
  const client: QueryCachePort = {
    invalidateQueries: (filters?: { queryKey?: readonly unknown[] }) => {
      calls.push({ op: 'invalidate', queryKey: filters?.queryKey });
    },
    removeQueries: (filters: { queryKey: readonly unknown[] }) => {
      calls.push({ op: 'remove', queryKey: filters.queryKey });
    },
    getQueryData: <T,>(key: readonly unknown[]) => key[0] === 'coves' ? coves as T | undefined : undefined,
    setQueryData: <T,>(key: readonly unknown[], value: T) => {
      calls.push({ op: 'set', queryKey: key });
      if (key[0] === 'coves') coves = value as typeof coves;
    },
    clear: () => { calls.push({ op: 'clear' }); },
  };
  return { calls, client, coves: () => coves };
}

describe('query invalidation adapter', () => {
  it('maps every planned key shape onto a queryKeys key and drops the rest', () => {
    expect(mapPlannedQueryKey(['coves'])).toEqual(queryKeys.coves());
    expect(mapPlannedQueryKey(['waves', 'cove', 'c1'])).toEqual(queryKeys.wavesInCove('c1'));
    expect(mapPlannedQueryKey(['wave', 'w1'])).toEqual(queryKeys.waveDetail('w1'));
    expect(mapPlannedQueryKey(['overlays', 'wave'])).toEqual(queryKeys.overlaysByKind('wave'));
    expect(mapPlannedQueryKey(['overlays', 'card'])).toEqual(queryKeys.overlaysByKind('card'));
    expect(mapPlannedQueryKey(['harness-items', 'card-1'])).toEqual(queryKeys.harnessItems('card-1'));
    expect(mapPlannedQueryKey(['spec-run', 'card-1'])).toEqual(queryKeys.specRun('card-1'));
    expect(mapPlannedQueryKey(['wave-report', 'w1'])).toEqual(queryKeys.waveReport('w1'));
    expect(mapPlannedQueryKey(['wave-report'])).toEqual(queryKeys.waveReportPrefix());
    for (const dropped of [['wave-files'], ['wave-files', 'w1'], ['waves-range'], ['wave-backlinks'], ['nope']]) {
      expect(mapPlannedQueryKey(dropped)).toBeNull();
    }
  });

  /*
   * The wave-report key is mapped in BOTH arities, and the bare one is the
   * point: dropping it — the treatment every other bare key gets — would leave
   * the TASKS panel dead for exactly the four events that change it.
   *
   * These four payloads are the ones the kernel actually emits, and they go
   * through `wireEventSchema` rather than a cast so a hand-written shape cannot
   * stand in for the wire. Note what they carry: `idempotency_key` is the task
   * id, and a task id is `"{wave_id}:{key}"`. The wave id is therefore present
   * in the bytes — the plan's `derivedWaveId` reads named fields only and does
   * not take an opaque id apart, which is why the *plan* cannot key these by
   * wave. "Carries no wave id at all" would be the wrong reason.
   */
  it.each([
    ['task.dispatched', { idempotency_key: 'w-7:alpha', kind: 'codex' }],
    ['task.completed', { idempotency_key: 'w-7:alpha', result: null, artifacts: [] }],
    ['task.failed', { idempotency_key: 'w-7:alpha', reason: 'gate red' }],
    ['task.gate_result', {
      task_id: 'w-7:alpha', idempotency_key: 'w-7:alpha', passed: false,
      log_tail: '', log_path: '/tmp/gate.log', attempt: 1,
    }],
  ] as const)('reaches the task-verdict cache for %s, whose wave id is only inside its task id', (ev, data) => {
    const event = wireEventSchema.parse({ ev, data });
    expect(event.data).toMatchObject({ idempotency_key: 'w-7:alpha' });
    const plan = invalidationPlanFor(event);
    const mapped = plan.invalidate.map(mapPlannedQueryKey).filter((key) => key !== null);
    expect(mapped).toContainEqual(queryKeys.waveReportPrefix());
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

  it('ignores lifecycle effects, which are not cache work', () => {
    const { calls, client } = recordingClient();
    const effects: EventEffect[] = [
      { type: 'persist-cursor', id: 7 },
      { type: 'reconnect' },
    ];
    applyEventEffects(client, effects);
    expect(calls).toEqual([]);
  });

  it('write-through replaces one existing cove and preserves every other row', () => {
    expectTypeOf<CacheWrite['value']>().toEqualTypeOf<CoveWire>();
    const old = { id: 'c1', name: 'old', color: '#111', sort: 1, kind: 'user', createdAt: 10, updatedAt: 20 } as const;
    const other = { id: 'c2', name: 'other', color: '#222', sort: 2, kind: 'user', createdAt: 11, updatedAt: 21 } as const;
    const { calls, client, coves } = recordingClient([old, other]);
    applyEventEffects(client, [{ type: 'write-through', writes: [{
      key: ['coves'], mode: 'replace-existing-cove',
      value: { id: 'c1', name: 'new', color: '#abc', sort: 3, kind: 'user', created_at: 10, updated_at: 30 },
    }] }]);
    expect(coves()).toEqual([
      { id: 'c1', name: 'new', color: '#abc', sort: 3, kind: 'user', createdAt: 10, updatedAt: 30 },
      other,
    ]);
    expect(calls).toEqual([{ op: 'set', queryKey: ['coves'] }]);
  });

  it('write-through never creates a phantom cove when the row is absent', () => {
    const existing = { id: 'c2', name: 'other', color: '#222', sort: 2, kind: 'user', createdAt: 11, updatedAt: 21 } as const;
    const { calls, client, coves } = recordingClient([existing]);
    applyEventEffects(client, [{ type: 'write-through', writes: [{
      key: ['coves'], mode: 'replace-existing-cove',
      value: { id: 'missing', name: 'phantom', color: '#abc', sort: 3, kind: 'user', created_at: 10, updated_at: 30 },
    }] }]);
    expect(coves()).toEqual([existing]);
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

  it('turns each real harness plan into its exact live query invalidations', () => {
    const expected = [
      ['harness.item.added', [queryKeys.harnessItems('card-1')]],
      ['harness.phase.changed', [queryKeys.specRun('card-1')]],
      ['harness.transcript.cleared', [queryKeys.harnessItems('card-1'), queryKeys.specRun('card-1')]],
      ['harness.user_message.enqueued', [queryKeys.harnessItems('card-1'), queryKeys.specRun('card-1')]],
    ] as const;
    for (const [ev, keys] of expected) {
      const plan = invalidationPlanFor({ ev, data: { card_id: 'card-1' } } as WireEvent);
      const { calls, client } = recordingClient();
      applyEventEffects(client, [{ type: 'invalidate', keys: plan.invalidate }]);
      expect(calls).toEqual(keys.map((queryKey) => ({ op: 'invalidate', queryKey })));
    }
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
