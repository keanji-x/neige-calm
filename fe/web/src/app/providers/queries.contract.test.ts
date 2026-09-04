// @vitest-environment jsdom
// Invariants owned by the shared query layer.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { createElement, type ReactNode } from 'react';
import { z } from 'zod';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { areaWireSchema, toArea } from '../../../../core/domain/area.ts';
import {
  readTrackReport, TRACK_REPORT_CARD_KIND, type TaskVerdict,
} from '../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY } from '../../../../core/domain/track.ts';
import {
  ApiError, areaListQueryOptions, harnessItemsQueryOptions, queryKeys, runOperation, taskVerdictsRefetchInterval,
  useAreaMutations, usePlannerMutations, useTrackMutations, useWorkspace, tracksInAreaQueryOptions,
} from './queries.ts';

function recordingTransport(reply: (request: ApiRequest) => ApiTransportResponse) {
  const paths: string[] = [];
  const transport: ApiTransportPort = {
    send(request) {
      paths.push(request.path);
      return Promise.resolve(reply(request));
    },
  };
  return { transport, paths };
}

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

const systemArea = { id: 'sys', name: 'system', color: '#000', sort: 0, kind: 'system', created_at: 1, updated_at: 1 };
const userArea = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const baseTrackWire = {
  id: 'w1', area_id: 'c1', title: 'Ship it', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};

describe('E2E-INV-SHELL-003 the system area never reaches the workspace surface', () => {
  it('filters a system area out of the list the shell renders', async () => {
    const { transport } = recordingTransport(() => ok([systemArea, userArea]));
    const areas = await areaListQueryOptions(transport, unauthorized).queryFn();
    expect(areas.map((area) => area.id)).toEqual(['c1']);
  });

  it('yields zero area rows for a fresh workspace that only has the system area', async () => {
    const { transport } = recordingTransport(() => ok([systemArea]));
    expect(await areaListQueryOptions(transport, unauthorized).queryFn()).toEqual([]);
  });

  it('orders the surviving areas by sort so the rail is stable', async () => {
    const { transport } = recordingTransport(() => ok([
      { ...userArea, id: 'b', sort: 3 }, { ...userArea, id: 'a', sort: 1 },
    ]));
    expect((await areaListQueryOptions(transport, unauthorized).queryFn()).map((area) => area.id)).toEqual(['a', 'b']);
  });
});

describe('failure channel', () => {
  it('requires callers to choose the unauthorized policy explicitly', () => {
    const transport: ApiTransportPort = { send: vi.fn() };
    if (transport.send === undefined) {
      // @ts-expect-error The channel/policy argument is deliberately mandatory.
      void runOperation(transport, { method: 'GET', path: '/private', responseSchema: z.unknown() });
    }
    expect(runOperation.length).toBe(3);
  });

  it('passes the transport unauthorized channel to every operation', async () => {
    const listener = vi.fn();
    const channel = createUnauthorizedChannel({ enqueue: (task) => task() });
    channel.subscribe(listener);
    const transport: ApiTransportPort = {
      send: () => Promise.resolve({ status: 401, statusText: 'Unauthorized', body: {} }),
    };
    await expect(runOperation(transport, { method: 'GET', path: '/private', responseSchema: z.unknown() }, channel)).rejects.toBeInstanceOf(ApiError);
    expect(listener).toHaveBeenCalledOnce();
  });
  it('rejects with ApiError carrying the normalized failure so Query can surface it', async () => {
    const { transport } = recordingTransport(() => ({ status: 500, statusText: 'Server Error', body: { code: 'boom', error: 'kaboom' } }));
    await expect(areaListQueryOptions(transport, unauthorized).queryFn()).rejects.toBeInstanceOf(ApiError);
  });

  it('keeps neutral tracks readable while exporting an overlay failure', async () => {
    const track = { id: 'w1', area_id: 'c1', title: 'Task', sort: 1, lifecycle: 'working', cwd: '/tmp',
      archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1 };
    const { transport } = recordingTransport((request) => {
      if (request.path === '/api/areas') return ok([userArea]);
      if (request.path === '/api/areas/c1/tracks') return ok([track]);
      return { status: 500, statusText: 'Server Error', body: { error: 'overlays down' } };
    });
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const wrapper = ({ children }: { children: ReactNode }) => createElement(QueryClientProvider, { client }, children);
    const { result } = renderHook(() => useWorkspace(transport, unauthorized), { wrapper });
    await waitFor(() => expect(result.current.tracks).toHaveLength(1));
    expect(result.current.areasError).toBeNull();
    expect(result.current.overlaysError).toBeInstanceOf(ApiError);
    expect(result.current.tracks[0]).toMatchObject(NEUTRAL_ACTIVITY);
  });

  it('rejects when the payload does not match the schema instead of rendering junk', async () => {
    const { transport } = recordingTransport(() => ok([{ id: 'c1' }]));
    await expect(areaListQueryOptions(transport, unauthorized).queryFn()).rejects.toBeInstanceOf(ApiError);
  });
});

describe('track list', () => {
  it('reads one area at a time so each area keeps its own cache entry', async () => {
    const { transport, paths } = recordingTransport(() => ok([]));
    await tracksInAreaQueryOptions(transport, 'c1', unauthorized).queryFn();
    await tracksInAreaQueryOptions(transport, 'c2', unauthorized).queryFn();
    expect(paths).toEqual(['/api/areas/c1/tracks', '/api/areas/c2/tracks']);
  });
});

describe('delete mutation wiring', () => {
  function mutationWrapper(client: QueryClient) {
    return ({ children }: { children: ReactNode }) =>
      createElement(QueryClientProvider, { client }, children);
  }

  it.each([
    ['track', (transport: ApiTransportPort) => useTrackMutations(transport, unauthorized),
      (mutations: ReturnType<typeof useTrackMutations>, signal: AbortSignal) => mutations.remove('w1', 'c1', signal)],
    ['area', (transport: ApiTransportPort) => useAreaMutations(transport, unauthorized),
      (mutations: ReturnType<typeof useAreaMutations>, signal: AbortSignal) => mutations.remove('c1', signal)],
  ] as const)('relays the caller signal through the real %s mutation operation', async (_kind, useMutations, remove) => {
    let requestSignal: AbortSignal | undefined;
    const transport: ApiTransportPort = { send: vi.fn((request: ApiRequest) => {
      requestSignal = request.signal as AbortSignal;
      return new Promise<ApiTransportResponse>((_resolve, reject) => requestSignal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError'))));
    }) };
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    const { result } = renderHook(() => useMutations(transport), { wrapper: mutationWrapper(client) });
    const controller = new AbortController();
    let pending!: Promise<void>;
    act(() => { pending = remove(result.current as never, controller.signal); });
    await waitFor(() => expect(requestSignal).toBe(controller.signal));
    controller.abort();
    await expect(pending).rejects.toBeInstanceOf(ApiError);
    expect(requestSignal?.aborted).toBe(true);
  });

  it('invalidates the track list even when an aborted delete may have committed', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    client.setQueryData(queryKeys.tracksInArea('c1'), [{ id: 'w1' }]);
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const transport: ApiTransportPort = { send: () => Promise.reject(new DOMException('aborted', 'AbortError')) };
    const { result } = renderHook(() => useTrackMutations(transport, unauthorized), { wrapper: mutationWrapper(client) });
    const controller = new AbortController();
    controller.abort();
    await expect(result.current.remove('w1', 'c1', controller.signal)).rejects.toBeInstanceOf(ApiError);
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.tracksInArea('c1') });
  });

  it('invalidates the area list even when an aborted delete may have committed', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    client.setQueryData(queryKeys.areas(), [userArea]);
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const transport: ApiTransportPort = { send: () => Promise.reject(new DOMException('aborted', 'AbortError')) };
    const { result } = renderHook(() => useAreaMutations(transport, unauthorized), { wrapper: mutationWrapper(client) });
    const controller = new AbortController();
    controller.abort();
    await expect(result.current.remove('c1', controller.signal)).rejects.toBeInstanceOf(ApiError);
    expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.areas() });
  });

  it('writes an Area PATCH response through before its background refetch', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    const original = toArea(areaWireSchema.parse(userArea));
    client.setQueryData(queryKeys.areas(), [original]);
    const updatedWire = {
      ...userArea,
      name: 'Studio',
      default_template_id: 'small-change',
      default_cwd: '/srv/studio',
      updated_at: original.updatedAt + 1,
    };
    const transport: ApiTransportPort = { send: () => Promise.resolve(ok(updatedWire)) };
    const { result } = renderHook(() => useAreaMutations(transport, unauthorized), {
      wrapper: mutationWrapper(client),
    });

    await act(() => result.current.update('c1', {
      name: 'Studio', default_template_id: 'small-change', default_cwd: '/srv/studio',
    }));

    expect(client.getQueryData<ReturnType<typeof toArea>[]>(queryKeys.areas())).toEqual([
      expect.objectContaining({
        name: 'Studio', defaultTemplateId: 'small-change', defaultCwd: '/srv/studio',
      }),
    ]);
  });

  it('invalidates Areas after a failed PATCH', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    client.setQueryData(queryKeys.areas(), [toArea(areaWireSchema.parse(userArea))]);
    let releaseRefetch!: () => void;
    const refetchHeld = new Promise<void>((resolve) => { releaseRefetch = resolve; });
    const invalidate = vi.spyOn(client, 'invalidateQueries').mockReturnValue(refetchHeld);
    const transport: ApiTransportPort = {
      send: () => Promise.reject(new Error('PATCH failed')),
    };
    const { result } = renderHook(() => useAreaMutations(transport, unauthorized), {
      wrapper: mutationWrapper(client),
    });

    const pending = result.current.update('c1', { name: 'Studio' });
    let settled = false;
    void pending.then(() => { settled = true; }, () => { settled = true; });
    await waitFor(() => expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.areas() }));
    expect(settled).toBe(false);
    releaseRefetch();
    await expect(pending).rejects.toBeInstanceOf(ApiError);
    expect(settled).toBe(true);
  });

  it('invalidates Areas after a failed POST that may already have committed', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    client.setQueryData(queryKeys.areas(), [toArea(areaWireSchema.parse(userArea))]);
    let releaseRefetch!: () => void;
    const refetchHeld = new Promise<void>((resolve) => { releaseRefetch = resolve; });
    const invalidate = vi.spyOn(client, 'invalidateQueries').mockReturnValue(refetchHeld);
    const transport: ApiTransportPort = {
      send: () => Promise.reject(new Error('POST response lost')),
    };
    const { result } = renderHook(() => useAreaMutations(transport, unauthorized), {
      wrapper: mutationWrapper(client),
    });

    const pending = result.current.create({ name: 'Reading', color: '#5B8DEF' });
    let settled = false;
    void pending.then(() => { settled = true; }, () => { settled = true; });
    await waitFor(() => expect(invalidate).toHaveBeenCalledWith({ queryKey: queryKeys.areas() }));
    expect(settled).toBe(false);
    releaseRefetch();
    await expect(pending).rejects.toBeInstanceOf(ApiError);
    expect(settled).toBe(true);
  });

  it('does not let a delayed Area PATCH response overwrite a newer event row', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    const original = toArea(areaWireSchema.parse(userArea));
    client.setQueryData(queryKeys.areas(), [original]);

    const responseWire = {
      ...userArea,
      name: 'My edit',
      default_template_id: 'small-change',
      default_cwd: '/srv/mine',
      updated_at: original.updatedAt + 1,
    };
    let releaseResponse!: () => void;
    const responseHeld = new Promise<void>((resolve) => { releaseResponse = resolve; });
    const send = vi.fn(async () => {
      await responseHeld;
      return ok(responseWire);
    });
    const transport: ApiTransportPort = { send };
    const { result } = renderHook(() => useAreaMutations(transport, unauthorized), {
      wrapper: mutationWrapper(client),
    });

    let pending!: ReturnType<typeof result.current.update>;
    act(() => {
      pending = result.current.update('c1', {
        name: 'My edit', default_template_id: 'small-change', default_cwd: '/srv/mine',
      });
    });
    await waitFor(() => expect(send).toHaveBeenCalledOnce());

    // `area_update_tx` assigns the later PATCH a strictly greater row version
    // even when both writes share one wall-clock millisecond. Its event reaches
    // this cache before the older HTTP response.
    const newerEvent = {
      ...original,
      name: 'Remote edit',
      defaultTemplateId: null,
      defaultCwd: '/srv/remote',
      updatedAt: responseWire.updated_at + 1,
    };
    act(() => { client.setQueryData(queryKeys.areas(), [newerEvent]); });
    releaseResponse();
    await act(async () => { await pending; });

    expect(client.getQueryData(queryKeys.areas())).toEqual([newerEvent]);
  });

  it('writes a created Area into an empty cache before the zero-state opener disappears', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    client.setQueryData(queryKeys.areas(), []);
    const createdWire = {
      ...userArea,
      id: 'c-new',
      name: 'Reading',
      default_template_id: null,
      default_cwd: null,
    };
    const transport: ApiTransportPort = { send: () => Promise.resolve(ok(createdWire)) };
    const { result } = renderHook(() => useAreaMutations(transport, unauthorized), {
      wrapper: mutationWrapper(client),
    });

    await act(() => result.current.create({ name: 'Reading', color: '#5B8DEF' }));

    expect(client.getQueryData<ReturnType<typeof toArea>[]>(queryKeys.areas()))
      .toEqual([expect.objectContaining({ id: 'c-new', name: 'Reading' })]);
  });

  it('does not let a delayed Area POST response overwrite a newer event row', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    client.setQueryData(queryKeys.areas(), []);
    const responseWire = {
      ...userArea,
      id: 'c-new',
      name: 'Reading',
      default_template_id: null,
      default_cwd: null,
      updated_at: 2,
    };
    let releaseResponse!: () => void;
    const responseHeld = new Promise<void>((resolve) => { releaseResponse = resolve; });
    const send = vi.fn(async () => {
      await responseHeld;
      return ok(responseWire);
    });
    const transport: ApiTransportPort = { send };
    const { result } = renderHook(() => useAreaMutations(transport, unauthorized), {
      wrapper: mutationWrapper(client),
    });

    let pending!: ReturnType<typeof result.current.create>;
    act(() => { pending = result.current.create({ name: 'Reading', color: '#5B8DEF' }); });
    await waitFor(() => expect(send).toHaveBeenCalledOnce());

    const newerEvent = {
      ...toArea(areaWireSchema.parse(responseWire)),
      name: 'Reading list',
      defaultTemplateId: 'small-change',
      defaultCwd: '/srv/reading',
      updatedAt: 3,
    };
    act(() => { client.setQueryData(queryKeys.areas(), [newerEvent]); });
    releaseResponse();
    await act(async () => { await pending; });

    expect(client.getQueryData(queryKeys.areas())).toEqual([newerEvent]);
  });

  it('does not promote one created Area into a complete list when the cache is uninitialized', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    const createdWire = {
      ...userArea,
      id: 'c-new',
      name: 'Reading',
      default_template_id: null,
      default_cwd: null,
    };
    const transport: ApiTransportPort = { send: () => Promise.resolve(ok(createdWire)) };
    const { result } = renderHook(() => useAreaMutations(transport, unauthorized), {
      wrapper: mutationWrapper(client),
    });

    await act(() => result.current.create({ name: 'Reading', color: '#5B8DEF' }));

    expect(client.getQueryData(queryKeys.areas())).toBeUndefined();
    expect(client.getQueryState(queryKeys.areas())).toBeUndefined();
  });

  it('preserves a failed no-data Area list instead of replacing it with one created row', async () => {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    await expect(client.fetchQuery({
      queryKey: queryKeys.areas(),
      queryFn: () => Promise.reject(new Error('areas unavailable')),
    })).rejects.toThrow('areas unavailable');
    const createdWire = {
      ...userArea,
      id: 'c-new',
      name: 'Reading',
      default_template_id: null,
      default_cwd: null,
    };
    const transport: ApiTransportPort = { send: () => Promise.resolve(ok(createdWire)) };
    const { result } = renderHook(() => useAreaMutations(transport, unauthorized), {
      wrapper: mutationWrapper(client),
    });

    await act(() => result.current.create({ name: 'Reading', color: '#5B8DEF' }));

    expect(client.getQueryData(queryKeys.areas())).toBeUndefined();
    expect(client.getQueryState(queryKeys.areas())?.status).toBe('error');
  });

  it('does not turn an acknowledged planner send into a send failure when refresh fails', async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const invalidate = vi.spyOn(client, 'invalidateQueries')
      .mockRejectedValue(new Error('history refresh failed'));
    const wireRuntimeKey = ['runtime', 'id'].join('_');
    const transport: ApiTransportPort = {
      send: () => Promise.resolve(ok({ card_id: 'card-1', [wireRuntimeKey]: 'runtime-1' })),
    };
    const { result } = renderHook(() => usePlannerMutations(transport, 'card-1', unauthorized), {
      wrapper: mutationWrapper(client),
    });

    await expect(result.current.send('accepted by the server')).resolves.toMatchObject({ card_id: 'card-1' });
    expect(invalidate).toHaveBeenCalledTimes(2);
  });
});

/*
 * ── What the card mutations do to the cache ────────────────────────────────
 *
 * The row the board draws comes out of `['track', trackId]`, and both of these
 * write it directly rather than waiting for the refetch they also queue. That is
 * not an optimisation in either direction:
 *
 *   * `removeCard` — the card's surface is unmounted by this write, and a
 *     terminal card left on screen for the length of a round-trip keeps a PTY
 *     attached to a card the kernel has already torn down.
 *   * the creates — the caller navigates to `?card=<id>` in the same tick, and
 *     the board can only draw a card the detail cache already holds.
 *
 * Nothing here is observing `['track', trackId]`, so the invalidation these
 * mutations queue cannot refetch: what the assertions read is the write itself.
 */
describe('card mutation cache writes', () => {
  const cardWire = (id: string) => ({
    id, track_id: 'w1', kind: 'terminal', title: null, sort: 1, payload: {},
    deletable: true, created_at: 1, updated_at: 2,
  });
  const detail = {
    track: { ...baseTrackWire }, cards: [cardWire('card-a'), cardWire('card-b')], overlays: [],
  };

  function mounted(transport: ApiTransportPort) {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    const wrapper = ({ children }: { children: ReactNode }) =>
      createElement(QueryClientProvider, { client }, children);
    const { result } = renderHook(() => useTrackMutations(transport, unauthorized), { wrapper });
    return { client, result };
  }

  it('drops the deleted row from the cached track detail without waiting for a refetch', async () => {
    let reads = 0;
    const transport: ApiTransportPort = {
      send: (request) => {
        if (request.method === 'GET') { reads += 1; return new Promise<ApiTransportResponse>(() => undefined); }
        return Promise.resolve(ok(undefined));
      },
    };
    const { client, result } = mounted(transport);
    client.setQueryData(queryKeys.trackDetail('w1'), detail);

    await act(() => result.current.removeCard('w1', 'card-a'));

    const after = client.getQueryData<typeof detail>(queryKeys.trackDetail('w1'));
    expect(after?.cards.map((card) => card.id)).toEqual(['card-b']);
    // And it was this write, not a re-read: the detail was never fetched at all.
    expect(reads).toBe(0);
  });

  it('leaves a detail it has no copy of alone rather than inventing one', async () => {
    const transport: ApiTransportPort = { send: () => Promise.resolve(ok(undefined)) };
    const { client, result } = mounted(transport);
    await act(() => result.current.removeCard('w1', 'card-a'));
    expect(client.getQueryData(queryKeys.trackDetail('w1'))).toBeUndefined();
  });

  it('writes a created card into the cached detail so the board can draw it at once', async () => {
    const created = cardWire('card-new');
    const transport: ApiTransportPort = {
      send: (request) => (request.method === 'POST'
        ? Promise.resolve(ok(created))
        : new Promise<ApiTransportResponse>(() => undefined)),
    };
    const { client, result } = mounted(transport);
    client.setQueryData(queryKeys.trackDetail('w1'), detail);

    await act(() => result.current.createCard('w1', { kind: 'file-viewer', payload: { path: '/x' } }));

    expect(client.getQueryData<typeof detail>(queryKeys.trackDetail('w1'))?.cards.map((card) => card.id))
      .toEqual(['card-a', 'card-b', 'card-new']);
  });

  /* A replayed create answers with a row the cache already holds. Appending it
     again would put the same card on the board twice. */
  it('does not duplicate a card the cached detail already carries', async () => {
    const transport: ApiTransportPort = {
      send: (request) => (request.method === 'POST'
        ? Promise.resolve(ok(cardWire('card-b')))
        : new Promise<ApiTransportResponse>(() => undefined)),
    };
    const { client, result } = mounted(transport);
    client.setQueryData(queryKeys.trackDetail('w1'), detail);

    await act(() => result.current.createCodex('w1', { theme: { fg: [0, 0, 0], bg: [1, 1, 1] } }));

    expect(client.getQueryData<typeof detail>(queryKeys.trackDetail('w1'))?.cards.map((card) => card.id))
      .toEqual(['card-a', 'card-b']);
  });
});

describe('track create folders cache', () => {
  it('drops a successful empty folders cache after attach_folder create', async () => {
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    client.setQueryData(queryKeys.areaFolders('c1'), []);
    const transport: ApiTransportPort = {
      send: () => Promise.resolve(ok({
        id: 'w1', area_id: 'c1', title: 'Ship', sort: 0, created_at: 1, updated_at: 1,
      })),
    };
    const wrapper = ({ children }: { children: ReactNode }) =>
      createElement(QueryClientProvider, { client }, children);
    const { result } = renderHook(() => useTrackMutations(transport, unauthorized), { wrapper });
    await act(() => result.current.create({
      area_id: 'c1',
      title: 'Ship',
      cwd: '/tmp/x',
      theme: { fg: [1, 2, 3], bg: [4, 5, 6] },
      attach_folder: true,
    }));
    expect(client.getQueryData(queryKeys.areaFolders('c1'))).toBeUndefined();
  });
});

describe('planner history pagination', () => {
  it('uses the first (oldest) row from the ascending first page as the second-page after_id', async () => {
    const firstPage = Array.from({ length: 300 }, (_, index) => ({
      id: 701 + index, runtime_id: 'runtime', card_id: 'card', track_id: 'track', thread_id: 'thread',
      turn_id: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
      params: '{}', created_at_ms: index,
    }));
    const { transport, paths } = recordingTransport(() => ok(firstPage));
    const options = harnessItemsQueryOptions(transport, 'card', unauthorized);
    const page = await options.queryFn({ pageParam: 0 });
    const cursor = options.getNextPageParam(page);
    expect(cursor).toBe(701);
    await options.queryFn({ pageParam: cursor ?? 0 });
    expect(paths[1]).toContain('after_id=701');
  });
});

/*
 * The track page's task-verdict timer, at the two states the query can be in.
 *
 * This is the whole convergence story for `worker_card_id`: the kernel's
 * `mark_running` stamps it without emitting anything, so if this callback
 * returns `false` at the wrong moment the panel's click-through to the worker
 * card is simply dead until the tab is reloaded.
 */
describe('task-verdict poll interval', () => {
  /*
   * The declarations are part of the input now, and that is the fix this
   * describe grew: the live branch asks whether the *panel* has a live row, not
   * whether the wire has a live verdict. The two differ for a verdict that
   * produces no row — see the last two cases.
   */
  const declaration = (id: string, key: string) => ({
    id, kind: 'task', rev: 1, payload: { key, kind: 'codex', declared_by: 'spec', ready: true, goal: 'g' },
  });
  const blocks = (declarations = [declaration('b-1', 'k')]) => readTrackReport([{
    id: 'c1', track_id: 'w1', kind: TRACK_REPORT_CARD_KIND, title: null, sort: 0,
    payload: { body: 'x', blocks: declarations }, deletable: false, created_at: 0, updated_at: 0,
  }])?.blocks ?? null;
  const interval = taskVerdictsRefetchInterval(blocks());
  const state = (over: Partial<{ data: TaskVerdict[]; errorUpdateCount: number }>) =>
    ({ state: { errorUpdateCount: 0, ...over } });
  const at = (status: string): TaskVerdict[] =>
    [{ blockId: 'b-1', key: 'k', schedulable: true, status, workerCardId: null }];

  it('polls while a run sits inside the eventless window', () => {
    expect(interval(state({ data: at('running') }))).toBe(3000);
  });

  it('stops once the read shows nothing left for a timer to find', () => {
    expect(interval(state({ data: at('done') }))).toBe(false);
    expect(interval(state({ data: [] }))).toBe(false);
  });

  /* A failed *refetch* keeps react-query's last good data, so this branch is
     unchanged by an error: a live run stays live and the timer that will fetch
     it again keeps running. */
  it('keeps polling a live run through a failed refetch, on the retained data', () => {
    expect(interval(state({ data: at('running'), errorUpdateCount: 3 })))
      .toBe(3000);
  });

  /*
   * The defect: when the FIRST load fails there is no data at all, so
   * `hasLiveTaskRun` is vacuously false and the timer never starts. Once
   * react-query has exhausted its retries nothing in the page ever asks again,
   * and a track that was mid-dispatch shows declaration words with no
   * click-through for as long as the tab stays open.
   */
  it('still schedules a retry when the initial load failed and there is no data', () => {
    expect(interval(state({ errorUpdateCount: 1 }))).toBe(15_000);
  });

  /* Bounded, because `GET /api/tracks/{id}/report` also fails *permanently*: a
     deleted track 404s and a track missing its `track-report` card 500s
     (`resolve_report_for_track`). An unconditional poll on "no data" would leave
     a stale tab hitting a dead route every few seconds forever. */
  it('gives up after a bounded number of failed loads rather than hammering a dead route', () => {
    expect(interval(state({ errorUpdateCount: 4 }))).toBe(15_000);
    expect(interval(state({ errorUpdateCount: 5 }))).toBe(false);
    expect(interval(state({ errorUpdateCount: 99 }))).toBe(false);
  });

  /* And no timer while the very first fetch is still on the wire — there is
     already a request in flight to wait for. */
  it('does not schedule anything before the first fetch has resolved', () => {
    expect(interval(state({}))).toBe(false);
  });

  /*
   * ── A live verdict that produces no row must not start the timer ─────────
   *
   * The kernel synthesises a verdict for a declaration that has been deleted
   * from the document — `blockId: ''`, naming no block this report has. No row
   * is built for it, so nothing on screen can converge, and a 3 s refetch on
   * its account is the unbounded cost this callback's comment says it does not
   * pay.
   *
   * The sibling case — a key two live declarations both claim — used to be
   * asserted here too, against a fixture where both verdicts carried
   * `status: 'running'`. The kernel no longer produces that shape: an
   * ambiguous key is answered `status: null` on every block that names it
   * (#1160), so the fixture would be testing a wire shape that cannot occur.
   * `report.test.ts` covers the reachable one.
   */
  it('does not poll for an in-flight run the report has no row for', () => {
    expect(interval(state({
      data: [{ blockId: '', key: 'deleted', schedulable: true, status: 'running', workerCardId: 'c-9' }],
    }))).toBe(false);
  });

  /* Premise for both of the above: the identical verdict on a declaration this
     report DOES have still polls, so the two are refusals and not a timer that
     stopped working. */
  it('still polls a live run whose declaration is in the document', () => {
    expect(taskVerdictsRefetchInterval(blocks([declaration('b-1', 'deleted')]))(state({
      data: [{ blockId: '', key: 'deleted', schedulable: true, status: 'running', workerCardId: 'c-9' }],
    }))).toBe(3000);
  });
});
