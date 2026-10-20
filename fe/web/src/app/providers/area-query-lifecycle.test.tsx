import { createElement, type ReactNode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ApiError } from '../../../../core/domain/failure-class.ts';
import { toArea, type AreaWire } from '../../../../core/domain/area.ts';
import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import { applyEventEffects } from '../events/query-invalidation-adapter.ts';
import { queryKeys, useAreaMutations, useWorkspace } from './queries.ts';

const area: AreaWire = { id: 'a1', name: 'Work', color: '#123456', sort: 0, kind: 'user', default_cwd: null, default_template_id: null, created_at: 1, updated_at: 1 };
const unauthorized = createUnauthorizedChannel({ enqueue: task => task() });
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
afterEach(cleanup);

it.each(['local', 'done', 'ws'] as const)('%s deletion retires mounted child queries before a delayed parent refresh and rejects old parent results', async source => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  client.setQueryData(queryKeys.areas(), [toArea(area)]);
  const parents: { request: ApiRequest; resolve: (reply: ApiTransportResponse) => void }[] = [];
  const children: ApiRequest[] = [];
  const transport: ApiTransportPort = { send: request => {
    if (request.method === 'DELETE') return Promise.resolve(source === 'done'
      ? { status: 404, statusText: 'Not Found', body: { error: 'gone' } } : { status: 204, statusText: 'No Content', body: undefined });
    if (request.path === '/api/areas') return new Promise(resolve => parents.push({ request, resolve }));
    if (request.path.includes('/tracks')) {
      children.push(request);
      return children.length === 1 ? Promise.resolve(ok([])) : new Promise(() => undefined);
    }
    return Promise.resolve(ok([]));
  } };
  const view = renderHook(() => ({ workspace: useWorkspace(transport, unauthorized), mutations: useAreaMutations(transport, unauthorized) }), {
    wrapper: ({ children }: { children: ReactNode }) => createElement(QueryClientProvider, { client }, children),
  });
  try {
    await waitFor(() => expect(view.result.current.workspace.tracksLoadingByArea.get('a1')).toBe(false));
    act(() => { void client.invalidateQueries({ queryKey: queryKeys.areas() }); void client.invalidateQueries({ queryKey: queryKeys.tracksInArea('a1') }); });
    await waitFor(() => { expect(parents).toHaveLength(1); expect(children).toHaveLength(2); });
    const parentCancelledAtWrite: boolean[] = [];
    const stopObserving = client.getQueryCache().subscribe(event => {
      const key = event.query.queryKey as readonly unknown[];
      if (event.type === 'updated' && key[0] === 'areas'
        && Array.isArray(event.query.state.data) && event.query.state.data.length === 0) {
        parentCancelledAtWrite.push(parents[0]?.request.signal?.aborted === true);
      }
    });
    await act(async () => {
      if (source === 'ws') {
        const plan = invalidationPlanFor({ ev: 'area.deleted', data: { id: 'a1' } });
        for (let duplicate = 0; duplicate < 2; duplicate++) {
          applyEventEffects(client, [{ type: 'write-through', writes: plan.writeThrough }, { type: 'invalidate', keys: plan.invalidate }]);
        }
      } else if (source === 'done') await expect(view.result.current.mutations.remove('a1')).rejects.toBeInstanceOf(ApiError);
      else await view.result.current.mutations.remove('a1');
    });
    await waitFor(() => expect(view.result.current.workspace.areas).toEqual([]));
    stopObserving();
    expect(parentCancelledAtWrite.length).toBeGreaterThan(0);
    expect(parentCancelledAtWrite.every(Boolean)).toBe(true);
    expect(parents[0]?.request.signal?.aborted).toBe(true);
    expect(children[1]?.signal?.aborted).toBe(true);
    await waitFor(() => expect(client.getQueryCache().find({ queryKey: queryKeys.tracksInArea('a1') })).toBeUndefined());
    await act(async () => { parents[0]?.resolve(ok([area])); await Promise.resolve(); });
    expect(client.getQueryData(queryKeys.areas())).toEqual([]);
    expect(children).toHaveLength(2);
    expect(view.result.current.workspace.areas).toEqual([]);
  } finally { view.unmount(); client.clear(); }
});

it.each([403, 500, 'lost'] as const)('retains the parent and child cache after an uncertain or refused delete (%s)', async status => {
  const client = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
  client.setQueryData(queryKeys.areas(), [toArea(area)]);
  client.setQueryData(queryKeys.tracksInArea('a1'), []);
  const transport: ApiTransportPort = { send: request => request.method === 'DELETE'
    ? status === 'lost' ? Promise.reject(new Error('response lost')) : Promise.resolve({ status, statusText: 'Failed', body: { error: 'failed' } })
    : new Promise(() => undefined) };
  const view = renderHook(() => ({ workspace: useWorkspace(transport, unauthorized), mutations: useAreaMutations(transport, unauthorized) }), {
    wrapper: ({ children }: { children: ReactNode }) => createElement(QueryClientProvider, { client }, children),
  });
  try {
    await act(async () => { await expect(view.result.current.mutations.remove('a1')).rejects.toThrow(); });
    expect(view.result.current.workspace.areas.map(row => row.id)).toEqual(['a1']);
    expect(client.getQueryData(queryKeys.tracksInArea('a1'))).toEqual([]);
  } finally { view.unmount(); client.clear(); }
});
