import { QueryClient, QueryObserver } from '@tanstack/react-query';
import { describe, expect, expectTypeOf, it, vi } from 'vitest';

import type { AreaWire } from '../../../../core/domain/area.ts';
import { wireEventSchema } from '../../../../core/api/schemas.ts';
import type { CacheWrite } from '../../../../core/events/invalidation-plan.ts';
import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import type { EventEffect } from '../../../../core/events/reducer.ts';
import type { ApiRequest, ApiTransportResponse, ApiTransportPort, ApiAbortSignal } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { trackDetailQueryOptions, trackConversationsQueryOptions, conversationHistoryQueryOptions, queryKeys } from '../providers/queries.ts';
import { applyEventEffects, mapPlannedQueryKey, type QueryCachePort } from './query-invalidation-adapter.ts';

type Call = Readonly<{ op: 'reset' | 'invalidate' | 'remove' | 'set' | 'clear'; queryKey?: readonly unknown[] }>;

function recordingClient(initialAreas?: readonly ReturnType<typeof import('../../../../core/domain/area.ts').toArea>[]) {
  const calls: Call[] = [];
  let areas = initialAreas;
  const client: QueryCachePort = {
    cancelQueries: () => Promise.resolve(),
    resetQueries: filters => { calls.push({ op: 'reset', queryKey: filters.queryKey }); },
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
    expect(mapPlannedQueryKey(['planner-conversations', 'card-1'])).toEqual(queryKeys.plannerConversations('card-1'));
    expect(mapPlannedQueryKey(['track-report', 'w1'])).toEqual(queryKeys.trackReport('w1'));
    expect(mapPlannedQueryKey(['track-report'])).toEqual(queryKeys.trackReportPrefix());
    expect(mapPlannedQueryKey(['track-conversations'])).toEqual(queryKeys.trackConversationsPrefix());
    expect(mapPlannedQueryKey(['track-conversations', 'w1'])).toEqual(queryKeys.trackConversations('w1'));
    expect(mapPlannedQueryKey(['conversation-initial-input'])).toEqual(['conversation-initial-input']);
    expect(mapPlannedQueryKey(['conversation-initial-input', 'w1'])).toEqual(['conversation-initial-input', 'w1']);
    expect(mapPlannedQueryKey(['conversation-initial-input', 'w1', 'card-1'])).toEqual(queryKeys.initialInput('w1', 'card-1'));
    expect(mapPlannedQueryKey(['today-launchpad'])).toEqual(queryKeys.todayLaunchpad());
    for (const dropped of [['conversation-initial-input', undefined], ['conversation-initial-input', 'w1', null], ['conversation-initial-input', 'w1', 'c1', 'extra'], ['track-files'], ['track-files', 'w1'], ['tracks-range'], ['track-backlinks'], ['nope']]) {
      expect(mapPlannedQueryKey(dropped)).toBeNull();
    }
  });

  it('releases accepted-input query leases for exactly the deleted card, deleted track and database scope', () => {
    const client = new QueryClient();
    const first = [...queryKeys.initialInput('w1', 'c1'), 'key-1', 'entry-1'];
    const second = [...queryKeys.initialInput('w1', 'c2'), 'key-2', 'entry-2'];
    const third = [...queryKeys.initialInput('w2', 'c3'), 'key-3', 'entry-3'];
    for (const key of [first, second, third]) client.setQueryData(key, { accepted: true });
    const card = invalidationPlanFor({ ev: 'card.deleted', data: { id: 'c1', track_id: 'w1' } });
    applyEventEffects(client, [{ type: 'remove', keys: card.remove }]);
    expect(client.getQueryData(first)).toBeUndefined();
    expect(client.getQueryData(second)).toEqual({ accepted: true });
    const track = invalidationPlanFor({ ev: 'track.deleted', data: { id: 'w1', area_id: 'area' } });
    applyEventEffects(client, [{ type: 'remove', keys: track.remove }]);
    expect(client.getQueryData(second)).toBeUndefined();
    expect(client.getQueryData(third)).toEqual({ accepted: true });
    applyEventEffects(client, [{ type: 'clear-cache' }]);
    expect(client.getQueryData(third)).toBeUndefined();
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

  it('invalidates each mapped key of an invalidate effect', async () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'invalidate', keys: [['areas'], ['track', 'w1'], ['track-files', 'w1']] }]);
    await settleReads();
    expect(calls).toEqual([
      { op: 'invalidate', queryKey: ['areas'] },
      { op: 'invalidate', queryKey: ['track', 'w1'] },
    ]);
  });

  it('invalidates the whole cache for a null key set after canceling track overlays', async () => {
    const { calls, client } = recordingClient();
    applyEventEffects(client, [{ type: 'invalidate', keys: null }]);
    await Promise.resolve();
    expect(calls).toEqual([{ op: 'reset', queryKey: queryKeys.transcriptPrefix() }, { op: 'invalidate', queryKey: undefined }]);
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
    expectTypeOf<Extract<CacheWrite, { mode: 'replace-existing-area' }>['value']>().toEqualTypeOf<AreaWire>();
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
  it('turns each real harness plan into its exact live query invalidations', async () => {
    const base = { worker_session_id: 'r-1', card_id: 'card-1', track_id: 'track-1' } as const;
    const expected = [
      [
        { ...base, item_db_id: 1, item_uuid: null, item_type: null, turn_id: null, method: 'x' },
        'harness.item.added', [queryKeys.harnessItems('card-1')],
      ],
      [
        { ...base, old_phase: 'idle', new_phase: 'turn_running' },
        'harness.phase.changed',
        [queryKeys.plannerRun('card-1'), queryKeys.harnessItems('card-1'), queryKeys.trackDetail('track-1'), queryKeys.trackConversations('track-1')],
      ],
      [
        { ...base, cleared_item_count: 12, cleared_params_bytes: 3400, card_age_ms_at_clear: 86400000 },
        'harness.transcript.cleared',
        [queryKeys.harnessItems('card-1'), queryKeys.plannerRun('card-1'), queryKeys.trackDetail('track-1'), queryKeys.trackConversations('track-1')],
      ],
      [
        { ...base, char_count: 3 }, 'harness.user_message.enqueued',
        [
          queryKeys.harnessItems('card-1'), queryKeys.plannerRun('card-1'),
          queryKeys.trackDetail('track-1'), queryKeys.trackConversations('track-1'),
        ],
      ],
    ] as const;
    for (const [data, ev, keys] of expected) {
      const plan = invalidationPlanFor(wireEventSchema.parse({ ev, data }));
      const { calls, client } = recordingClient();
      applyEventEffects(client, [{ type: 'invalidate', keys: plan.invalidate }]);
      const expectedCalls = [...keys, queryKeys.initialInput('track-1', 'card-1')].map((queryKey) => ({ op: 'invalidate', queryKey }));
      await vi.waitFor(() => {
        expect(calls).toHaveLength(expectedCalls.length);
        expect(calls).toEqual(expect.arrayContaining(expectedCalls));
      });
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
      { op: 'invalidate', queryKey: queryKeys.overlaysByKind('card') },
      { op: 'invalidate', queryKey: queryKeys.trackDetail('track-1') },
      { op: 'invalidate', queryKey: queryKeys.trackReport('track-1') },
      { op: 'invalidate', queryKey: queryKeys.trackConversations('track-1') },
      { op: 'invalidate', queryKey: queryKeys.initialInput('track-1', 'card-1') },
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
      { op: 'invalidate', queryKey: queryKeys.trackReportPrefix() },
      { op: 'invalidate', queryKey: queryKeys.trackConversationsPrefix() },
      { op: 'invalidate', queryKey: queryKeys.initialInputPrefix() },
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
      { op: 'remove', queryKey: queryKeys.initialInputTrack('w1') },
      { op: 'invalidate', queryKey: queryKeys.overlaysByKind('track') },
      { op: 'invalidate', queryKey: queryKeys.trackReportPrefix() },
    ]);
  });
});

// Real production query options decode server metadata; the transport captures the server value
// at request start so releasing an obsolete response cannot accidentally read the new value.
function metadataRead(surface: 'detail' | 'list', respectsAbort = false, trackId = 'track-1') {
  let title: string | null = 'Original';
  const reads: { request: ApiRequest; release: () => void }[] = [];
  const transport = { send: (request: ApiRequest) => new Promise<ApiTransportResponse>((resolve, reject) => {
    const body = surface === 'detail' ? {
      track: { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/tmp', frozen_at: null }, id: trackId, area_id: 'area-1', title: 'Track', sort: 0, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 },
      can_reopen: false, can_admin: false, can_close: true, overlays: [],
      cards: [{ id: 'card-1', track_id: trackId, kind: 'codex', title, sort: 0, payload: { planner_harness: true }, deletable: true, created_at: 1, updated_at: 2 }],
    } : [{ id: 'card-1', trackId, title, kind: 'track-assistant', provider: 'codex', state: 'idle', updatedAt: 2, archivedAt: null, lastTurnCompletedAt: null }];
    reads.push({ request, release: () => resolve({ status: 200, statusText: 'OK', body }) });
    if (respectsAbort) request.signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')), { once: true });
  }) };
  const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
  const options = surface === 'detail' ? trackDetailQueryOptions(transport, trackId, unauthorized)
    : trackConversationsQueryOptions(transport, trackId, unauthorized);
  return { reads, options, setTitle: (next: string | null) => { title = next; },
    title: (client: QueryClient) => {
      const data = client.getQueryData(options.queryKey);
      return surface === 'detail' ? (data as { cards: { title: string | null }[] })?.cards[0]?.title
        : (data as { title: string | null }[])?.[0]?.title;
    } };
}

function inputEvent(kind: 'deleted' | 'edited' | 'cleared' | 'persisted' | 'presentation', trackId = 'track-1') {
  const base = { card_id: 'card-1', track_id: trackId, worker_session_id: 'runtime' };
  return wireEventSchema.parse(kind === 'presentation'
    ? { ev: 'harness.presentation.changed', data: { card_id: 'card-1', track_id: trackId, cursor: { contract: 'conversation_v1', card_id: 'card-1', source: 'presentation', position: 1 } } }
    : kind === 'cleared'
    ? { ev: 'harness.transcript.cleared', data: { ...base, cleared_item_count: 1, cleared_params_bytes: 20, card_age_ms_at_clear: 100 } }
    : kind === 'persisted'
      ? { ev: 'harness.item.added', data: { ...base, item_db_id: 1, item_uuid: null, item_type: 'userMessage', turn_id: null, method: 'item/completed' } }
      : { ev: 'harness.queue.changed', data: { ...base, entry_id: 'first', change: kind, actor: { kind: 'User' } } });
}

function refreshInput(client: QueryClient, kind: Parameters<typeof inputEvent>[0], trackId = 'track-1') {
  applyEventEffects(client, [{ type: 'invalidate', keys: invalidationPlanFor(inputEvent(kind, trackId)).invalidate }]);
}

// Let cancellation/invalidation continuations and a held transport response settle without timers
// that could cause a query's retry/poll policy to supply an unrelated second event.
async function settleReads() { for (let i = 0; i < 20; i++) await Promise.resolve(); }

for (const surface of ['detail', 'list'] as const) {
  for (const respectsAbort of [false, true]) {
    it.each(['deleted', 'edited', 'cleared', 'persisted', 'presentation'] as const)(
      `replaces initial ${surface} metadata for %s (transport abort=${respectsAbort})`, async kind => {
        const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
        const read = metadataRead(surface, respectsAbort);
        const observer = new QueryObserver<unknown>(client, read.options);
        const unsubscribe = observer.subscribe(() => undefined);
        try {
          expect(read.reads).toHaveLength(1);
          expect(client.getQueryData(read.options.queryKey)).toBeUndefined();
          const next = kind === 'edited' || kind === 'persisted' ? 'New input' : null;
          read.setTitle(next);
          refreshInput(client, kind);
          await settleReads();
          read.reads[1]?.release();
          await settleReads();
          // Obsolete response arrives last, including when the transport ignores abort.
          read.reads[0].release();
          await settleReads();
          expect({ reads: read.reads.length, title: read.title(client), invalidated: client.getQueryState(read.options.queryKey)?.isInvalidated }).toEqual({ reads: 2, title: next, invalidated: false });
          expect(read.reads[0].request.signal?.aborted).toBe(true);
        } finally { unsubscribe(); client.clear(); }
      },
    );
  }

  it(`keeps adjacent ${surface} input events scoped and the latest replacement authoritative`, async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const read = metadataRead(surface);
    const foreign = metadataRead(surface, false, 'foreign');
    const release = new QueryObserver<unknown>(client, read.options).subscribe(() => undefined);
    const releaseForeign = new QueryObserver<unknown>(client, foreign.options).subscribe(() => undefined);
    try {
      read.setTitle('Edited');
      refreshInput(client, 'edited');
      await settleReads();
      read.setTitle(null);
      refreshInput(client, 'deleted');
      refreshInput(client, 'cleared'); // Same-tick adjacent events also share only a post-event read.
      await settleReads();
      read.reads.at(-1)?.release();
      await settleReads();
      for (const stale of read.reads.slice(0, -1).reverse()) stale.release();
      foreign.reads[0].release();
      await settleReads();
      expect(read.reads).toHaveLength(3);
      expect(read.title(client)).toBeNull();
      expect(foreign.reads).toHaveLength(1);
      expect(foreign.reads[0].request.signal?.aborted).toBe(false);
      expect(foreign.title(client)).toBe('Original');
      expect(client.getQueryCache().getAll().map(query => query.queryKey)).toEqual([read.options.queryKey, foreign.options.queryKey]);
    } finally { release(); releaseForeign(); client.clear(); }
  });

  it(`marks inactive ${surface} metadata stale without eagerly reading it`, async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const read = metadataRead(surface);
    try {
      const first = client.fetchQuery<unknown>(read.options);
      read.reads[0].release();
      await first;
      read.setTitle(null);
      refreshInput(client, 'deleted');
      await settleReads();
      expect(read.reads).toHaveLength(1);
      expect(client.getQueryState(read.options.queryKey)?.isInvalidated).toBe(true);
      const next = client.fetchQuery<unknown>(read.options);
      read.reads[1].release();
      await next;
      expect(read.title(client)).toBeNull();
    } finally { client.clear(); }
  });
}
it('presentation event replaces an in-flight old empty conversation page before its late response', async () => {
  const client=new QueryClient({defaultOptions:{queries:{retry:false}}});
  const reads:Array<{finish:(value:ApiTransportResponse)=>void;signal:ApiAbortSignal|undefined}>=[];
  const transport:ApiTransportPort={send:request=>new Promise(resolve=>reads.push({finish:resolve,signal:request.signal}))};
  const unauthorized=createUnauthorizedChannel({enqueue:task=>task()});
  const options=conversationHistoryQueryOptions(transport,'card',unauthorized);
  const observer=new QueryObserver(client,{...options,queryFn:({signal})=>options.queryFn({position:{kind:'latest'},signal})});
  const release=observer.subscribe(()=>undefined);
  await vi.waitFor(()=>expect(reads).toHaveLength(1));
  const cursor={contract:'conversation_v1',card_id:'card',source:'presentation',position:1} as const;
  const event=wireEventSchema.parse({ev:'harness.presentation.changed',data:{card_id:'card',track_id:'track',cursor}});
  applyEventEffects(client,[{type:'invalidate',keys:invalidationPlanFor(event).invalidate}]);
  await vi.waitFor(()=>expect(reads).toHaveLength(2));
  expect(reads[0].signal?.aborted).toBe(true);
  const page={contract:'conversation_v1',source:'presentation',conversation: 0, items:[{cursor,source:{kind:'human',message_id:'message',revision:1,
    author:{user_id:'jian',display_name:'Jian'},text:'already saved',attachments:[],created_at_ms:1}}],next_cursor:null};
  reads[1].finish({status:200,statusText:'OK',body:page});
  await vi.waitFor(()=>expect(observer.getCurrentResult().data).toEqual(page));
  reads[0].finish({status:200,statusText:'OK',body:{...page,items:[]}});
  await Promise.resolve();await Promise.resolve();
  expect(observer.getCurrentResult().data).toEqual(page);
  release();client.clear();
});
