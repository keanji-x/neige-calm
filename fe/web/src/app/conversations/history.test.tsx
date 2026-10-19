// @vitest-environment jsdom
import { createElement, type ReactNode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { wireEventSchema } from '../../../../core/api/schemas.ts';
import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import { applyEventEffects } from '../events/query-invalidation-adapter.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { harnessItemsQueryOptions as pageOptions } from '../providers/queries.ts';
import { earlierReadingPosition, historyRows, latestHistory, observedWindow, useTranscriptReads, useTranscriptHistory, type History } from './history.ts';

type Row = History['latest']['rows'][number];
function rows(first: number, count = 60): Row[] {
  return Array.from({ length: count }, (_, i) => ({ id: first + i, card_id: 'card', track_id: 'track',
    worker_session_id: 'worker', thread_id: 'thread', turn_id: null, turn_error_text: null,
    item_uuid: null, item_type: 'agentMessage', method: 'item/completed', params: '{}', created_at_ms: first + i }));
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
function setup() {
  const paths: string[] = [];
  let newest = rows(61);
  let earlier = rows(1);
  let held: ReturnType<typeof deferred<Row[]>> | null = null;
  const options = pageOptions({ send: async request => {
    paths.push(request.path);
    const body = request.path.includes('after_id=0&') ? newest : await (held?.promise ?? earlier);
    return { status: 200, statusText: 'OK', body };
  } }, 'card', createUnauthorizedChannel({ enqueue: task => task() }));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  let sequence = 0;
  const nextRead = () => ++sequence;
  const mount = () => renderHook(() => {
    const reads = useTranscriptReads(nextRead);
    return { history: useTranscriptHistory(reads.track(options), true, reads), reads };
  }, { wrapper: ({ children }: { children: ReactNode }) => createElement(QueryClientProvider, { client }, children) });
  const view = mount();
  return { ...view, mount, client, paths, key: options.queryKey,
    newest: (value: Row[]) => { newest = value; }, earlier: (value: Row[]) => { earlier = value; },
    hold: () => { held = deferred<Row[]>(); return held; } };
}
afterEach(cleanup);

it('refreshes latest facts with two cached windows without replaying old HTTP and preserves the earlier read fence', async () => {
  const view = setup();
  await waitFor(() => expect(view.result.current.history.isSuccess).toBe(true));
  const before = view.result.current.reads.startOf(view.result.current.history.data);
  act(() => { expect(view.result.current.history.loadEarlier()).toBe(true); expect(view.result.current.history.loadEarlier()).toBe(false); });
  await waitFor(() => expect(historyRows(view.result.current.history.data)).toHaveLength(120));
  expect(view.result.current.reads.startOf(view.result.current.history.data)).toBe(before);
  const older = view.result.current.history.data!.retained[0].rows;
  view.newest(rows(62));
  await act(async () => { await view.client.invalidateQueries({ queryKey: view.key }); });
  await waitFor(() => expect(historyRows(view.result.current.history.data).map(row => row.id)).toEqual(rows(1, 121).map(row => row.id)));
  expect(historyRows(view.result.current.history.data)[0]).toBe(older[0]);
  expect(view.paths.map(path => new URL(path, 'http://test').searchParams.get('after_id'))).toEqual(['0', '61', '0']);
  expect(view.result.current.reads.startOf(view.result.current.history.data)).toBeGreaterThan(before);
  view.client.clear();
});

it('fills a rollover gap only on input and then continues at the retained oldest cursor', async () => {
  const view = setup();
  await waitFor(() => expect(view.result.current.history.isSuccess).toBe(true));
  view.newest(rows(181));
  await act(async () => { await view.client.invalidateQueries({ queryKey: view.key }); });
  await waitFor(() => expect(historyRows(view.result.current.history.data)).toHaveLength(120));
  expect(view.result.current.history.earlier.cursor).toBe(181);
  expect(view.paths).toHaveLength(2);
  // Sparse row ids: read through an actual overlap rather than assuming adjacent ids prove continuity.
  view.earlier(rows(120));
  act(() => { expect(view.result.current.history.loadEarlier()).toBe(true); });
  await waitFor(() => expect(historyRows(view.result.current.history.data)).toHaveLength(179));
  expect(view.result.current.history.earlier.cursor).toBe(61);
  view.client.clear();
});

it('discards an earlier response superseded by an equal-payload latest read', async () => {
  const view = setup();
  await waitFor(() => expect(view.result.current.history.isSuccess).toBe(true));
  const held = view.hold();
  act(() => { expect(view.result.current.history.loadEarlier()).toBe(true); });
  await waitFor(() => expect(view.paths).toHaveLength(2));
  await act(async () => { await view.client.invalidateQueries({ queryKey: view.key }); });
  await waitFor(() => expect(view.result.current.history.isFetching).toBe(false));
  expect(view.result.current.history.earlier.pending).toBe(false);
  act(() => { held.resolve(rows(1)); });
  await waitFor(() => expect(view.result.current.history.earlier.pending).toBe(false));
  expect(historyRows(view.result.current.history.data)).toHaveLength(60);
  view.client.clear();
});

it('surfaces an expired nonadvancing cursor and retries only on explicit input', async () => {
  const view = setup();
  await waitFor(() => expect(view.result.current.history.isSuccess).toBe(true));
  view.earlier(rows(61));
  act(() => { view.result.current.history.loadEarlier(); });
  await waitFor(() => expect(view.result.current.history.earlier.failed).toBe(true));
  expect(view.result.current.history.error?.message).toContain('did not advance');
  expect(view.paths).toHaveLength(2);
  view.earlier(rows(1));
  act(() => { expect(view.result.current.history.loadEarlier()).toBe(true); });
  await waitFor(() => expect(historyRows(view.result.current.history.data)).toHaveLength(120));
  expect(view.result.current.history.error).toBeNull();
  view.client.clear();
});

it('applies latest interval edits and deletions without duplicate or unordered rows', () => {
  const previous: History = { latest: observedWindow(rows(61), { kind: 'latest' }), retained: [observedWindow(rows(1), { kind: 'before', rowId: 61 })] };
  const latest = rows(62, 61).filter(row => row.id !== 100).map(row => row.id === 101 ? { ...row, params: 'edited' } : row);
  const data = latestHistory(previous, latest);
  const result = historyRows(data);
  expect(result.map(row => row.id)).toEqual(rows(1, 122).filter(row => row.id !== 100).map(row => row.id));
  expect(result.find(row => row.id === 101)?.params).toBe('edited');
  expect(earlierReadingPosition(data)).toEqual({ kind: 'before', rowId: 1 });
  expect(historyRows(latestHistory(data, []))).toEqual([]);
  expect(historyRows(latestHistory(data, rows(10, 3))).map(row => row.id)).toEqual([10, 11, 12]);
});

it('bounds retained snapshots by loaded windows through repeated one-row rollovers', () => {
  let data: History = { latest: observedWindow(rows(61), { kind: 'latest' }), retained: [observedWindow(rows(1), { kind: 'before', rowId: 61 })] };
  for (let first = 62; first <= 110; first++) data = latestHistory(data, rows(first));
  expect(data.retained).toHaveLength(1);
  expect(data.retained[0].rows.length).toBeGreaterThan(60);
  expect(data.retained[0].coverage.lower).toEqual({ kind: 'from', rowId: 1 });
  expect(historyRows(data).map(row => row.id)).toEqual(rows(1, 169).map(row => row.id));
  expect(earlierReadingPosition(data)).toEqual({ kind: 'before', rowId: 1 });
});

it.each([0, 3])('records the actual earlier short response as read-to-start (%i rows)', async count => {
  const view = setup();
  await waitFor(() => expect(view.result.current.history.isSuccess).toBe(true));
  const latestRead = view.result.current.reads.startOf(view.result.current.history.data);
  view.earlier(rows(10, count));
  act(() => { expect(view.result.current.history.loadEarlier()).toBe(true); });
  await waitFor(() => expect(view.result.current.history.earlier.cursor).toBeNull());
  expect(view.result.current.history.data?.retained[0].coverage).toEqual({
    upper: { kind: 'before', rowId: 61 }, lower: { kind: 'start' },
  });
  expect(historyRows(view.result.current.history.data).map(row => row.id)).toEqual([...rows(10, count), ...rows(61)].map(row => row.id));
  expect(view.result.current.reads.startOf(view.result.current.history.data)).toBe(latestRead);
  expect(view.paths).toHaveLength(2);
  view.client.clear();
});

it.each(['clear', 'rewind', 'restored', 'replay'] as const)('expires retained windows and a pending old cursor on %s', async kind => {
  const view = setup();
  await waitFor(() => expect(view.result.current.history.isSuccess).toBe(true));
  act(() => { view.result.current.history.loadEarlier(); });
  await waitFor(() => expect(historyRows(view.result.current.history.data)).toHaveLength(120));
  const held = view.hold();
  act(() => { view.result.current.history.loadEarlier(); });
  await waitFor(() => expect(view.paths).toHaveLength(3));
  // Still a full latest page: reset must expire older rows even when a short page cannot prove it.
  view.newest(rows(181));
  act(() => {
    if (kind === 'replay') applyEventEffects(view.client, [{ type: 'invalidate', keys: null }]);
    else {
      const data = { worker_session_id: 'worker', card_id: 'card', track_id: 'track' };
      const event = wireEventSchema.parse(kind === 'restored'
        ? { ev: 'harness.queue.changed', data: { ...data, entry_id: 'input', change: 'restored', actor: { kind: 'Kernel' } } } : kind === 'clear'
        ? { ev: 'harness.transcript.cleared', data: { ...data, cleared_item_count: 120, cleared_params_bytes: 0, card_age_ms_at_clear: 1 } }
        : { ev: 'harness.transcript.rewound', data: { ...data, turn_id: 'turn', removed_item_count: 120 } });
      const plan = invalidationPlanFor(event);
      applyEventEffects(view.client, [{ type: 'write-through', writes: plan.writeThrough }, { type: 'invalidate', keys: plan.invalidate }]);
    }
  });
  await waitFor(() => expect(historyRows(view.result.current.history.data).map(row => row.id)).toEqual(rows(181).map(row => row.id)));
  act(() => { held.resolve(rows(1)); });
  await waitFor(() => expect(view.result.current.history.earlier.pending).toBe(false));
  expect(historyRows(view.result.current.history.data).map(row => row.id)).toEqual(rows(181).map(row => row.id));
  view.client.clear();
});

it('retires an old cursor error when a real latest read supersedes it without replaying that cursor', async () => {
  const view = setup();
  await waitFor(() => expect(view.result.current.history.isSuccess).toBe(true));
  view.earlier(rows(61));
  act(() => { view.result.current.history.loadEarlier(); });
  await waitFor(() => expect(view.result.current.history.earlier.failed).toBe(true));
  view.newest(rows(181));
  await act(async () => { await view.client.invalidateQueries({ queryKey: view.key }); });
  await waitFor(() => expect(view.result.current.history.earlier.cursor).toBe(181));
  expect(view.result.current.history.error).toBeNull();
  expect(view.result.current.history.earlier.failed).toBe(false);
  expect(view.paths.map(path => new URL(path, 'http://test').searchParams.get('after_id'))).toEqual(['0', '61', '0']);
  view.client.clear();
});

it('keeps a remounted owner’s equal latest read ahead of an earlier response from the old owner', async () => {
  const view = setup();
  await waitFor(() => expect(view.result.current.history.isSuccess).toBe(true));
  const held = view.hold();
  act(() => { expect(view.result.current.history.loadEarlier()).toBe(true); });
  await waitFor(() => expect(view.paths).toHaveLength(2));
  view.unmount();
  const reopened = view.mount();
  await waitFor(() => expect(view.paths).toHaveLength(3));
  await waitFor(() => expect(reopened.result.current.history.isFetching).toBe(false));
  act(() => { held.resolve(rows(1)); });
  await waitFor(() => expect(view.client.isMutating()).toBe(0));
  expect(historyRows(reopened.result.current.history.data)).toHaveLength(60);
  expect(view.paths.map(path => new URL(path, 'http://test').searchParams.get('after_id'))).toEqual(['0', '61', '0']);
  reopened.unmount(); view.client.clear();
});

it.each([false, true])('allows current-window input after latest supersession (old request settled: %s)', async settled => {
  const view = setup();
  await waitFor(() => expect(view.result.current.history.isSuccess).toBe(true));
  const held = view.hold();
  act(() => { expect(view.result.current.history.loadEarlier()).toBe(true); });
  await waitFor(() => expect(view.paths).toHaveLength(2));
  view.newest(rows(181));
  await act(async () => { await view.client.invalidateQueries({ queryKey: view.key }); });
  await waitFor(() => expect(view.result.current.history.earlier.cursor).toBe(181));
  expect(view.result.current.history.isFetching).toBe(false);
  if (settled) { held.resolve(rows(1)); await waitFor(() => expect(view.client.isMutating()).toBe(0)); }
  let admitted = false;
  act(() => { admitted = view.result.current.history.loadEarlier(); });
  held.resolve(rows(1));
  await waitFor(() => expect(view.client.isMutating()).toBe(0));
  view.unmount(); view.client.clear();
  expect(admitted).toBe(true);
});

// Exercise the actual shared mutation cache with independent read owners, including
// owners that did not number the snapshot currently stored by the other observer.
it('serializes one effective window across observers and shares pending/error/retry state', async () => {
  const view = setup();
  await waitFor(() => expect(view.result.current.history.isSuccess).toBe(true));
  const sibling = view.mount();
  await waitFor(() => expect(view.paths).toHaveLength(2));
  await waitFor(() => expect(sibling.result.current.history.isFetching).toBe(false));
  const held = view.hold();
  act(() => {
    expect(view.result.current.history.loadEarlier()).toBe(true);
    expect(sibling.result.current.history.loadEarlier()).toBe(false);
    expect(view.result.current.history.loadEarlier()).toBe(false);
  });
  await waitFor(() => expect(view.paths).toHaveLength(3));
  await waitFor(() => expect(sibling.result.current.history.earlier.pending).toBe(true));
  act(() => { held.reject(new Error('current earlier failed')); });
  await waitFor(() => expect(sibling.result.current.history.error?.message).toBe('Transport request failed'));
  expect(view.result.current.history.earlier.failed).toBe(true);
  const retry = view.hold();
  act(() => {
    expect(sibling.result.current.history.loadEarlier()).toBe(true);
    expect(view.result.current.history.loadEarlier()).toBe(false);
  });
  await waitFor(() => expect(view.paths).toHaveLength(4));
  await waitFor(() => expect(view.result.current.history.earlier.pending).toBe(true));
  expect(view.result.current.history.error).toBeNull();
  const fence = sibling.result.current.reads.startOf(sibling.result.current.history.data);
  act(() => { retry.resolve(rows(1)); });
  await waitFor(() => expect(historyRows(view.result.current.history.data)).toHaveLength(120));
  expect(sibling.result.current.reads.startOf(sibling.result.current.history.data)).toBe(fence);
  expect(sibling.result.current.history.error).toBeNull();
  sibling.unmount(); view.unmount(); view.client.clear();
});

it.each(['rollover', 'equal', 'remount', 'reset', 'ABA'] as const)(
  'admits one new page after %s while rejecting obsolete completion and error', async transition => {
    const view = setup();
    await waitFor(() => expect(view.result.current.history.isSuccess).toBe(true));
    const old = view.hold();
    act(() => { expect(view.result.current.history.loadEarlier()).toBe(true); });
    await waitFor(() => expect(view.paths).toHaveLength(2));
    let current = view;
    if (transition === 'remount') {
      view.unmount();
      current = { ...view, ...view.mount() };
      await waitFor(() => expect(view.paths).toHaveLength(3));
      await waitFor(() => expect(current.result.current.history.isFetching).toBe(false));
    } else {
      if (transition !== 'equal') view.newest(rows(181));
      await act(async () => {
        if (transition === 'reset') await view.client.resetQueries({ queryKey: view.key });
        else await view.client.invalidateQueries({ queryKey: view.key });
      });
      if (transition === 'ABA') {
        view.newest(rows(61));
        await act(async () => { await view.client.invalidateQueries({ queryKey: view.key }); });
      }
    }
    await waitFor(() => expect(current.result.current.history.earlier.pending).toBe(false));
    const snapshot = current.result.current.history.data;
    const cursor = current.result.current.history.earlier.cursor!;
    const fence = current.result.current.reads.startOf(snapshot);
    expect(current.result.current.history.earlier.pending).toBe(false);
    expect(current.result.current.history.error).toBeNull();
    const active = view.hold();
    const before = view.paths.length;
    act(() => {
      expect(current.result.current.history.loadEarlier()).toBe(true);
      expect(current.result.current.history.loadEarlier()).toBe(false);
    });
    await waitFor(() => expect(view.paths).toHaveLength(before + 1));
    await waitFor(() => expect(current.result.current.history.earlier.pending).toBe(true));
    // A stale success must neither publish a snapshot nor release the active lock.
    act(() => { old.resolve(rows(1)); });
    await waitFor(() => expect(view.client.isMutating()).toBe(1));
    expect(current.result.current.history.data).toBe(snapshot);
    expect(current.result.current.reads.startOf(snapshot)).toBe(fence);
    expect(current.result.current.history.earlier.pending).toBe(true);
    act(() => { expect(current.result.current.history.loadEarlier()).toBe(false); });
    act(() => { active.resolve(rows(cursor - 60)); });
    await waitFor(() => expect(current.result.current.history.earlier.pending).toBe(false));
    expect(current.result.current.reads.startOf(current.result.current.history.data)).toBe(fence);
    expect(historyRows(current.result.current.history.data).map(row => row.id)).toContain(cursor - 60);

    // Now retire another request, admit its successor, and reject the obsolete HTTP.
    const staleError = view.hold();
    act(() => { expect(current.result.current.history.loadEarlier()).toBe(true); });
    await waitFor(() => expect(view.client.isMutating()).toBe(1));
    await act(async () => { await view.client.invalidateQueries({ queryKey: view.key }); });
    await waitFor(() => expect(current.result.current.history.earlier.pending).toBe(false));
    const latest = current.result.current.history.data;
    const latestFence = current.result.current.reads.startOf(latest);
    expect(current.result.current.history.earlier.pending).toBe(false);
    const successor = view.hold();
    act(() => { expect(current.result.current.history.loadEarlier()).toBe(true); });
    await waitFor(() => expect(view.client.isMutating()).toBe(2));
    act(() => { staleError.reject(new Error('obsolete earlier failed')); });
    await waitFor(() => expect(view.client.isMutating()).toBe(1));
    expect(current.result.current.history.data).toBe(latest);
    expect(current.result.current.reads.startOf(latest)).toBe(latestFence);
    expect(current.result.current.history.error).toBeNull();
    expect(current.result.current.history.earlier.failed).toBe(false);
    expect(current.result.current.history.earlier.pending).toBe(true);
    act(() => { expect(current.result.current.history.loadEarlier()).toBe(false); });
    act(() => { successor.resolve([]); });
    await waitFor(() => expect(view.client.isMutating()).toBe(0));
    expect(current.result.current.reads.startOf(current.result.current.history.data)).toBe(latestFence);
    current.unmount(); view.client.clear();
  },
);
