import { QueryClient, QueryObserver } from '@tanstack/react-query';
import { describe, expect, expectTypeOf, it, vi } from 'vitest';

import type { AreaWire } from '../../../../core/domain/area.ts';
import { wireEventSchema } from '../../../../core/api/schemas.ts';
import type { CacheWrite } from '../../../../core/events/invalidation-plan.ts';
import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import type { EventEffect } from '../../../../core/events/reducer.ts';
import { queryKeys } from '../providers/queries.ts';
import { applyEventEffects, mapPlannedQueryKey, type QueryCachePort } from './query-invalidation-adapter.ts';

type Call = Readonly<{ op: 'invalidate' | 'remove' | 'set' | 'clear'; queryKey?: readonly unknown[] }>;

function recordingClient(initialAreas?: readonly ReturnType<typeof import('../../../../core/domain/area.ts').toArea>[]) {
  const calls: Call[] = [];
  let areas = initialAreas;
  const client: QueryCachePort = {
    cancelQueries: () => Promise.resolve(),
    invalidateQueries: (filters?: { queryKey?: readonly unknown[] }) => {
      calls.push({ op: 'invalidate', queryKey: filters?.queryKey });
    },
    removeQueries: (filters: { queryKey: readonly unknown[] }) => {
      calls.push({ op: 'remove', queryKey: filters.queryKey });
    },
    getQueryData: <T,>(key: readonly unknown[]) => key[0] === 'areas' ? areas as T | undefined : undefined,
    setQueryData: <T,>(key: readonly unknown[], value: T) => {
      calls.push({ op: 'set', queryKey: key });
      if (key[0] === 'areas') areas = value as typeof areas;
    },
    clear: () => { calls.push({ op: 'clear' }); },
  };
  return { calls, client, areas: () => areas };
}

describe('query invalidation adapter', () => {
  it('maps every planned key shape onto a queryKeys key and drops the rest', () => {
    expect(mapPlannedQueryKey(['areas'])).toEqual(queryKeys.areas());
    expect(mapPlannedQueryKey(['tracks', 'area', 'c1'])).toEqual(queryKeys.tracksInArea('c1'));
    expect(mapPlannedQueryKey(['track', 'w1'])).toEqual(queryKeys.trackDetail('w1'));
    expect(mapPlannedQueryKey(['overlays', 'track'])).toEqual(queryKeys.overlaysByKind('track'));
    expect(mapPlannedQueryKey(['overlays', 'card'])).toEqual(queryKeys.overlaysByKind('card'));
    expect(mapPlannedQueryKey(['harness-items', 'card-1'])).toEqual(queryKeys.harnessItems('card-1'));
    expect(mapPlannedQueryKey(['planner-run', 'card-1'])).toEqual(queryKeys.plannerRun('card-1'));
    expect(mapPlannedQueryKey(['track-report', 'w1'])).toEqual(queryKeys.trackReport('w1'));
    expect(mapPlannedQueryKey(['track-report'])).toEqual(queryKeys.trackReportPrefix());
    expect(mapPlannedQueryKey(['track-conversations'])).toEqual(queryKeys.trackConversationsPrefix());
    expect(mapPlannedQueryKey(['track-conversations', 'w1'])).toEqual(queryKeys.trackConversations('w1'));
    expect(mapPlannedQueryKey(['today-launchpad'])).toEqual(queryKeys.todayLaunchpad());
    for (const dropped of [['track-files'], ['track-files', 'w1'], ['tracks-range'], ['track-backlinks'], ['nope']]) {
      expect(mapPlannedQueryKey(dropped)).toBeNull();
    }
  });

  /* Both links are silent when they break: an unmapped key is discarded here without a warning. */
  it('turns a report edit into a refresh of the Today document and its resolve', () => {
    const event = wireEventSchema.parse({
      ev: 'track.report_edited',
      data: {
        track_id: 'lp', card_id: 'report-card', author: 'assistant', edit_id: 'edit-1',
        summary_before: '', summary_after: 'today', body_before: '', body_after: '# today',
      },
    });
    const mapped = invalidationPlanFor(event).invalidate
      .map(mapPlannedQueryKey)
      .filter((key) => key !== null);
    expect(mapped).toContainEqual(queryKeys.todayLaunchpad());
    expect(mapped).toContainEqual(queryKeys.trackDetail('lp'));
  });

  /* The bare track-report key is the point: dropping it would leave the TASKS panel dead for these
   * events. Parsed through `wireEventSchema`; the track id is only inside the opaque task id. */
  it.each([
    ['task.dispatched', { idempotency_key: 'w-7:alpha', kind: 'codex' }],
    ['task.completed', { idempotency_key: 'w-7:alpha', result: null, artifacts: [] }],
    ['task.failed', { idempotency_key: 'w-7:alpha', reason: 'gate red' }],
    ['task.gate_result', {
      task_id: 'w-7:alpha', idempotency_key: 'w-7:alpha', passed: false,
      log_tail: '', log_path: '/tmp/gate.log', attempt: 1,
    }],
  ] as const)('reaches the task-verdict cache for %s, whose track id is only inside its task id', (ev, data) => {
    const event = wireEventSchema.parse({ ev, data });
    expect(event.data).toMatchObject({ idempotency_key: 'w-7:alpha' });
    const plan = invalidationPlanFor(event);
    const mapped = plan.invalidate.map(mapPlannedQueryKey).filter((key) => key !== null);
    expect(mapped).toContainEqual(queryKeys.trackReportPrefix());
  });

  it('maps a task-plan cancellation event to its track report query', () => {
    const event = wireEventSchema.parse({
      ev: 'plan.updated',
      data: { track_id: 'w-7', changed_keys: ['alpha'], agent_message: 'canceled' },
    });
    const mapped = invalidationPlanFor(event).invalidate
      .map(mapPlannedQueryKey)
      .filter((key) => key !== null);
    expect(mapped).toEqual([queryKeys.trackReport('w-7')]);
  });

  it('never emits a key the built surface does not define', () => {
    // The mapped area-list key must be the very key the area query registers,
    // otherwise an invalidation silently refreshes nothing.
    expect(mapPlannedQueryKey(['areas'])).toEqual(['areas']);
    expect(mapPlannedQueryKey(['tracks', 'area', 'c1'])).toEqual(['tracks', 'c1']);
  });

  it('invalidates each mapped key of an invalidate effect', () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'invalidate', keys: [['areas'], ['track', 'w1'], ['track-files', 'w1']] }]);
    expect(calls).toEqual([
      { op: 'invalidate', queryKey: ['areas'] },
      { op: 'invalidate', queryKey: ['track', 'w1'] },
    ]);
  });

  it('invalidates the whole cache for a null key set after canceling track overlays', async () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'invalidate', keys: null }]);
    await Promise.resolve();
    expect(calls).toEqual([{ op: 'invalidate', queryKey: undefined }]);
  });

  it.each([
    ['mapped track overlay', [{ type: 'invalidate', keys: [['overlays', 'track']] }] as const],
    ['replay-complete full invalidation', [{ type: 'invalidate', keys: null }] as const],
  ])('aborts an active track-overlay read before starting its replacement for %s', async (_name, effects) => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const starts: string[] = [];
    const signals: AbortSignal[] = [];
    const observer = new QueryObserver(client, {
      queryKey: queryKeys.overlaysByKind('track'),
      queryFn: ({ signal }) => {
        const index = signals.length;
        signals.push(signal);
        starts.push(`start-${index + 1}`);
        if (index > 0) return Promise.resolve([]);
        return new Promise<never>((_resolve, reject) => {
          signal.addEventListener('abort', () => {
            starts.push('abort-1');
            reject(new DOMException('aborted', 'AbortError'));
          }, { once: true });
        });
      },
    });
    const unsubscribe = observer.subscribe(() => undefined);
    await vi.waitFor(() => expect(signals).toHaveLength(1));

    applyEventEffects(client, effects);

    await vi.waitFor(() => expect(signals).toHaveLength(2));
    expect(signals[0]?.aborted).toBe(true);
    expect(starts).toEqual(['start-1', 'abort-1', 'start-2']);
    unsubscribe();
  });

  it('does not cancel an active card-overlay read when refreshing track overlays', async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const cardSignals: AbortSignal[] = [];
    const cardObserver = new QueryObserver(client, {
      queryKey: queryKeys.overlaysByKind('card'),
      queryFn: ({ signal }) => {
        cardSignals.push(signal);
        return new Promise<never>(() => undefined);
      },
    });
    const unsubscribe = cardObserver.subscribe(() => undefined);
    await vi.waitFor(() => expect(cardSignals).toHaveLength(1));

    applyEventEffects(client, [{ type: 'invalidate', keys: [['overlays', 'track']] }]);
    await Promise.resolve();

    expect(cardSignals[0]?.aborted).toBe(false);
    unsubscribe();
  });

  it('clears the cache for a clear-cache effect', () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'clear-cache' }]);
    expect(calls).toEqual([{ op: 'clear' }]);
  });

  it('removes mapped keys for a remove effect', () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'remove', keys: [['track', 'w1'], ['tracks-range']] }]);
    expect(calls).toEqual([{ op: 'remove', queryKey: ['track', 'w1'] }]);
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

  it('write-through replaces one existing area and preserves every other row', () => {
    expectTypeOf<CacheWrite['value']>().toEqualTypeOf<AreaWire>();
    const old = {
      id: 'c1', name: 'old', color: '#111', sort: 1, kind: 'user',
      defaultTemplateId: null, defaultCwd: null, createdAt: 10, updatedAt: 20,
    } as const;
    const other = {
      id: 'c2', name: 'other', color: '#222', sort: 2, kind: 'user',
      defaultTemplateId: null, defaultCwd: null, createdAt: 11, updatedAt: 21,
    } as const;
    const { calls, client, areas } = recordingClient([old, other]);
    applyEventEffects(client, [{ type: 'write-through', writes: [{
      key: ['areas'], mode: 'replace-existing-area',
      value: {
        id: 'c1', name: 'new', color: '#abc', sort: 3, kind: 'user',
        default_template_id: 'small-change', default_cwd: '/srv/work', created_at: 10, updated_at: 30,
      },
    }] }]);
    expect(areas()).toEqual([
      {
        id: 'c1', name: 'new', color: '#abc', sort: 3, kind: 'user',
        defaultTemplateId: 'small-change', defaultCwd: '/srv/work', createdAt: 10, updatedAt: 30,
      },
      other,
    ]);
    expect(calls).toEqual([{ op: 'set', queryKey: ['areas'] }]);
  });

  it('write-through ignores an older area event that arrives after a newer cache version', () => {
    const current = {
      id: 'c1', name: 'newer', color: '#111', sort: 1, kind: 'user',
      defaultTemplateId: 'small-change', defaultCwd: '/srv/new', createdAt: 10, updatedAt: 30,
    } as const;
    const { client, areas } = recordingClient([current]);
    applyEventEffects(client, [{ type: 'write-through', writes: [{
      key: ['areas'], mode: 'replace-existing-area',
      value: {
        id: 'c1', name: 'older', color: '#abc', sort: 3, kind: 'user',
        default_template_id: 'investigation', default_cwd: '/srv/old', created_at: 10, updated_at: 20,
      },
    }] }]);
    expect(areas()).toEqual([current]);
  });

  it('write-through applies an equal-version area event for historical replay compatibility', () => {
    const current = {
      id: 'c1', name: 'before', color: '#111', sort: 1, kind: 'user',
      defaultTemplateId: null, defaultCwd: null, createdAt: 10, updatedAt: 20,
    } as const;
    const { client, areas } = recordingClient([current]);
    applyEventEffects(client, [{ type: 'write-through', writes: [{
      key: ['areas'], mode: 'replace-existing-area',
      value: {
        id: 'c1', name: 'after', color: '#abc', sort: 3, kind: 'user',
        default_template_id: 'small-change', default_cwd: '/srv/work', created_at: 10, updated_at: 20,
      },
    }] }]);
    expect(areas()?.[0]).toMatchObject({
      name: 'after', defaultTemplateId: 'small-change', defaultCwd: '/srv/work', updatedAt: 20,
    });
  });

  it('write-through never creates a phantom area when the row is absent', () => {
    const existing = {
      id: 'c2', name: 'other', color: '#222', sort: 2, kind: 'user',
      defaultTemplateId: null, defaultCwd: null, createdAt: 11, updatedAt: 21,
    } as const;
    const { calls, client, areas } = recordingClient([existing]);
    applyEventEffects(client, [{ type: 'write-through', writes: [{
      key: ['areas'], mode: 'replace-existing-area',
      value: {
        id: 'missing', name: 'phantom', color: '#abc', sort: 3, kind: 'user',
        default_template_id: null, default_cwd: null, created_at: 10, updated_at: 30,
      },
    }] }]);
    expect(areas()).toEqual([existing]);
    expect(calls).toEqual([]);
  });

  it('write-through leaves an uninitialized area cache absent', () => {
    const { calls, client, areas } = recordingClient();
    applyEventEffects(client, [{ type: 'write-through', writes: [{
      key: ['areas'], mode: 'replace-existing-area',
      value: {
        id: 'missing', name: 'phantom', color: '#abc', sort: 3, kind: 'user',
        default_template_id: null, default_cwd: null, created_at: 10, updated_at: 30,
      },
    }] }]);
    expect(areas()).toBeUndefined();
    expect(calls).toEqual([]);
  });

  it('preserves effect order across a snapshot-required style batch', () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [
      { type: 'clear-cache' },
      { type: 'invalidate', keys: [['areas']] },
      { type: 'remove', keys: [['track', 'w1']] },
    ]);
    expect(calls.map((call) => call.op)).toEqual(['clear', 'invalidate', 'remove']);
  });

  it('turns a real area.updated plan into an area-list invalidation', () => {
    const area = {
      id: 'c1', name: 'Area', color: '#fff', sort: 0, kind: 'user',
      default_template_id: null, default_cwd: null, created_at: 1, updated_at: 2,
    } as const;
    const plan = invalidationPlanFor({ ev: 'area.updated', data: area });
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'invalidate', keys: plan.invalidate }]);
    expect(calls).toEqual([{ op: 'invalidate', queryKey: queryKeys.areas() }]);
  });

  /* Parsed through `wireEventSchema`, not cast: `track_id` is required on the wire, and a `{ card_id }`
   * stub once planned `['track-conversations', undefined]` and froze the wrong expectation. */
  it('turns each real harness plan into its exact live query invalidations', () => {
    const base = { worker_session_id: 'r-1', card_id: 'card-1', track_id: 'track-1' } as const;
    const expected = [
      [
        { ...base, item_db_id: 1, item_uuid: null, item_type: null, turn_id: null, method: 'x' },
        'harness.item.added', [queryKeys.harnessItems('card-1')],
      ],
      [
        { ...base, old_phase: 'idle', new_phase: 'turn_running' },
        'harness.phase.changed',
        [queryKeys.plannerRun('card-1'), queryKeys.harnessItems('card-1'), queryKeys.trackConversations('track-1'), queryKeys.trackDetail('track-1')],
      ],
      [
        { ...base, cleared_item_count: 12, cleared_params_bytes: 3400, card_age_ms_at_clear: 86400000 },
        'harness.transcript.cleared',
        [queryKeys.harnessItems('card-1'), queryKeys.plannerRun('card-1')],
      ],
      [
        { ...base, char_count: 3 }, 'harness.user_message.enqueued',
        [
          queryKeys.harnessItems('card-1'), queryKeys.plannerRun('card-1'),
          queryKeys.trackConversations('track-1'),
        ],
      ],
    ] as const;
    for (const [data, ev, keys] of expected) {
      const plan = invalidationPlanFor(wireEventSchema.parse({ ev, data }));
      const { calls, client } = recordingClient();
      applyEventEffects(client, [{ type: 'invalidate', keys: plan.invalidate }]);
      expect(calls).toEqual(keys.map((queryKey) => ({ op: 'invalidate', queryKey })));
    }
  });

  /* Exact call lists, not `toContainEqual`: "the key is in there somewhere" is what let the seam open. */
  it('drives the conversation-list keys all the way onto the query client', async () => {
    const event = wireEventSchema.parse({
      ev: 'worker_session.started',
      data: {
        worker_session_id: 'r-1', card_id: 'card-1', kind: 'codex',
        agent_provider: 'codex', status: 'starting',
      },
    });
    const plan = invalidationPlanFor(event, { findTrackOwningCard: () => 'track-1' });
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'invalidate', keys: plan.invalidate }]);
    await Promise.resolve();
    expect(calls).toEqual([
      { op: 'invalidate', queryKey: queryKeys.trackDetail('track-1') },
      { op: 'invalidate', queryKey: queryKeys.overlaysByKind('card') },
      { op: 'invalidate', queryKey: queryKeys.trackConversations('track-1') },
      { op: 'invalidate', queryKey: queryKeys.trackReport('track-1') },
    ]);
  });

  /* The bare prefix arity must reach the client too, or an open list stays stale for the transitions that move a row's `state`. */
  it('drives the bare track-conversations prefix onto the client when no track resolves', async () => {
    const event = wireEventSchema.parse({
      ev: 'worker_session.status_changed',
      data: { worker_session_id: 'r-1', card_id: 'card-1', old_status: 'starting', new_status: 'running' },
    });
    const plan = invalidationPlanFor(event, { findTrackOwningCard: () => null });
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'invalidate', keys: plan.invalidate }]);
    await Promise.resolve();
    expect(calls).toEqual([
      { op: 'invalidate', queryKey: queryKeys.overlaysByKind('card') },
      { op: 'invalidate', queryKey: queryKeys.trackConversationsPrefix() },
      { op: 'invalidate', queryKey: queryKeys.trackReportPrefix() },
    ]);
  });

  it('turns a real track.deleted plan into tree diagnostics refresh plus ordinary cleanup', async () => {
    const plan = invalidationPlanFor({
      ev: 'track.deleted',
      data: { id: 'w1', area_id: 'c1' },
    });
    const { calls, client } = recordingClient();
    applyEventEffects(client, [
      { type: 'invalidate', keys: plan.invalidate },
      { type: 'remove', keys: plan.remove },
    ]);
    await Promise.resolve();
    expect(calls).toEqual([
      { op: 'invalidate', queryKey: queryKeys.tracksInArea('c1') },
      { op: 'remove', queryKey: queryKeys.trackDetail('w1') },
      { op: 'remove', queryKey: queryKeys.trackReport('w1') },
      { op: 'invalidate', queryKey: queryKeys.overlaysByKind('track') },
      { op: 'invalidate', queryKey: queryKeys.trackReportPrefix() },
    ]);
  });
});
