// Unit tests for `useOverlayState` — the synced replacement for `useState`.
//
// Coverage (per design doc §6.2):
//
//   1. Initial render returns `default`.
//   2. After mock GET resolves, returns the server value.
//   3. Setter invokes POST with the new value; optimistic value visible
//      synchronously (before the network resolves).
//   4. Server-confirmed payload writes through onSuccess; subsequent
//      eventBridge `overlay.set` invalidate is exercised separately in
//      `eventBridge.test.tsx`.
//   5. Mock POST rejects → optimistic rolls back to previous.
//   6. Setter functional form `(prev) => next` sees the correct prev.
//
// We mock `api/calm.ts` wholesale (same pattern as
// `web/src/api/queries.test.tsx`); the boundary we're testing is hook
// behavior, not network plumbing.

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

vi.mock('../api/calm', () => ({
  listOverlays: vi.fn(),
  upsertOverlay: vi.fn(),
}));

import * as api from '../api/calm';
import { overlayStateQueryKey, useOverlayState } from './useOverlayState';
import type { KernelOverlay } from '../api/wire';

type LayoutValue = { positions: Record<string, { x: number; y: number; w: number; h: number }> };

function makeOverlay(payload: LayoutValue): KernelOverlay {
  return {
    id: 'ov-1',
    plugin_id: 'kernel',
    entity_kind: 'view',
    entity_id: 'w1',
    kind: 'layout',
    payload: payload as unknown,
    updated_at: 0,
  };
}

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0 },
      mutations: { retry: false },
    },
  });
}

function wrapper(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

const DEFAULT: LayoutValue = { positions: {} };

beforeEach(() => {
  vi.clearAllMocks();
});

describe('useOverlayState', () => {
  it('returns the default value on initial render (query pending)', async () => {
    // Never-resolving promise — the hook should still surface the default
    // synchronously rather than blocking on the network.
    (api.listOverlays as ReturnType<typeof vi.fn>).mockReturnValue(
      new Promise(() => {}),
    );

    const client = makeClient();
    const { result } = renderHook(
      () =>
        useOverlayState<LayoutValue>({
          entity_kind: 'view',
          entity_id: 'w1',
          kind: 'layout',
          default: DEFAULT,
        }),
      { wrapper: wrapper(client) },
    );

    expect(result.current[0]).toEqual(DEFAULT);
  });

  it('returns the server value after the GET resolves', async () => {
    const server: LayoutValue = {
      positions: { 'card-a': { x: 0, y: 0, w: 4, h: 3 } },
    };
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeOverlay(server),
    ]);

    const client = makeClient();
    const { result } = renderHook(
      () =>
        useOverlayState<LayoutValue>({
          entity_kind: 'view',
          entity_id: 'w1',
          kind: 'layout',
          default: DEFAULT,
        }),
      { wrapper: wrapper(client) },
    );

    await waitFor(() => expect(result.current[0]).toEqual(server));
    expect(api.listOverlays).toHaveBeenCalledWith('view', 'w1');
  });

  it('filters out overlays of other kinds / plugins', async () => {
    const target: LayoutValue = {
      positions: { c: { x: 1, y: 1, w: 2, h: 2 } },
    };
    // Two overlays at the same entity — only the matching one should
    // surface as our state.
    const wrongKind: KernelOverlay = {
      ...makeOverlay({ positions: {} }),
      id: 'other',
      kind: 'status',
      payload: { state: 'idle' } as unknown,
    };
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([
      wrongKind,
      makeOverlay(target),
    ]);

    const client = makeClient();
    const { result } = renderHook(
      () =>
        useOverlayState<LayoutValue>({
          entity_kind: 'view',
          entity_id: 'w1',
          kind: 'layout',
          default: DEFAULT,
        }),
      { wrapper: wrapper(client) },
    );

    await waitFor(() => expect(result.current[0]).toEqual(target));
  });

  it('setter triggers POST and the optimistic value is visible synchronously', async () => {
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    // Make the mutation hang so we can observe the optimistic state
    // before the POST resolves.
    let resolvePost: ((o: KernelOverlay) => void) | undefined;
    const postPromise = new Promise<KernelOverlay>((resolve) => {
      resolvePost = resolve;
    });
    (api.upsertOverlay as ReturnType<typeof vi.fn>).mockReturnValue(postPromise);

    const client = makeClient();
    const { result } = renderHook(
      () =>
        useOverlayState<LayoutValue>({
          entity_kind: 'view',
          entity_id: 'w1',
          kind: 'layout',
          default: DEFAULT,
        }),
      { wrapper: wrapper(client) },
    );
    // Wait for the initial GET to flush — otherwise the optimistic write
    // races the empty-list resolution and the test asserts the wrong
    // post-write value.
    await waitFor(() => expect(api.listOverlays).toHaveBeenCalled());

    const next: LayoutValue = {
      positions: { c: { x: 2, y: 0, w: 3, h: 2 } },
    };
    act(() => {
      result.current[1](next);
    });

    // Optimistic value visible without waiting for the POST itself —
    // the setter calls `setQueryData` synchronously, then RQ notifies
    // observers in a microtask (`waitFor` collapses to a single tick
    // when the assertion already passes — see vitest docs).
    await waitFor(() => expect(result.current[0]).toEqual(next));
    expect(api.upsertOverlay).toHaveBeenCalledWith({
      plugin_id: 'kernel',
      entity_kind: 'view',
      entity_id: 'w1',
      kind: 'layout',
      payload: next,
    });

    // Resolve the POST with a slightly different server value so we can
    // assert onSuccess writes through. (In reality the server echoes the
    // submitted payload, but a server-mutated echo is the more general
    // case.)
    const echoed: LayoutValue = {
      positions: { c: { x: 2, y: 0, w: 3, h: 2 }, 'normalized-by-server': { x: 0, y: 0, w: 1, h: 1 } },
    };
    await act(async () => {
      resolvePost!(makeOverlay(echoed));
      await postPromise;
    });
    await waitFor(() => expect(result.current[0]).toEqual(echoed));
  });

  it('rolls back the optimistic value when the POST rejects', async () => {
    const seeded: LayoutValue = {
      positions: { a: { x: 0, y: 0, w: 1, h: 1 } },
    };
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeOverlay(seeded),
    ]);
    // Reject *asynchronously* — `mockRejectedValue` rejects on the very
    // next microtask, which on some test runs lands before our
    // synchronous optimistic-visible assertion. A deferred-rejection
    // promise keeps the optimistic window open until we explicitly
    // resolve it below.
    let rejectPost: ((e: Error) => void) | undefined;
    const postPromise = new Promise<never>((_resolve, reject) => {
      rejectPost = reject;
    });
    (api.upsertOverlay as ReturnType<typeof vi.fn>).mockReturnValue(postPromise);

    const client = makeClient();
    const { result } = renderHook(
      () =>
        useOverlayState<LayoutValue>({
          entity_kind: 'view',
          entity_id: 'w1',
          kind: 'layout',
          default: DEFAULT,
        }),
      { wrapper: wrapper(client) },
    );
    await waitFor(() => expect(result.current[0]).toEqual(seeded));

    // Silence the expected console.error from onError so it doesn't
    // pollute the test output.
    const consoleErr = vi.spyOn(console, 'error').mockImplementation(() => {});
    const next: LayoutValue = {
      positions: { a: { x: 5, y: 5, w: 2, h: 2 } },
    };
    act(() => {
      result.current[1](next);
    });
    // Optimistic value visible after RQ flushes its observer
    // notification microtask. The mutation is still pending (we
    // haven't rejected the promise yet), so the optimistic value
    // remains until we explicitly reject below.
    await waitFor(() => expect(result.current[0]).toEqual(next));

    // Now reject — onError rolls back to `seeded`.
    await act(async () => {
      rejectPost!(new Error('boom'));
      // Swallow the unhandled-rejection warning vitest would otherwise
      // print; we want the rejection observed by the mutation only.
      await postPromise.catch(() => {});
    });
    await waitFor(() => expect(result.current[0]).toEqual(seeded));
    expect(consoleErr).toHaveBeenCalled();
    consoleErr.mockRestore();
  });

  it("setter's functional form sees the correct `prev`", async () => {
    const initial: LayoutValue = {
      positions: { a: { x: 0, y: 0, w: 1, h: 1 } },
    };
    (api.listOverlays as ReturnType<typeof vi.fn>).mockResolvedValue([
      makeOverlay(initial),
    ]);
    (api.upsertOverlay as ReturnType<typeof vi.fn>).mockImplementation(
      (body: { payload: LayoutValue }) =>
        Promise.resolve(makeOverlay(body.payload)),
    );

    const client = makeClient();
    const { result } = renderHook(
      () =>
        useOverlayState<LayoutValue>({
          entity_kind: 'view',
          entity_id: 'w1',
          kind: 'layout',
          default: DEFAULT,
        }),
      { wrapper: wrapper(client) },
    );
    await waitFor(() => expect(result.current[0]).toEqual(initial));

    let observedPrev: LayoutValue | null = null;
    act(() => {
      result.current[1]((prev) => {
        observedPrev = prev;
        return {
          positions: { ...prev.positions, b: { x: 4, y: 0, w: 2, h: 2 } },
        };
      });
    });
    expect(observedPrev).toEqual(initial);
    await waitFor(() =>
      expect(result.current[0]).toEqual({
        positions: {
          a: { x: 0, y: 0, w: 1, h: 1 },
          b: { x: 4, y: 0, w: 2, h: 2 },
        },
      }),
    );
  });

  it('writes through the canonical query key shape', async () => {
    expect(overlayStateQueryKey('kernel', 'view', 'w1', 'layout')).toEqual([
      'overlay',
      'kernel',
      'view',
      'w1',
      'layout',
    ]);
  });

  it('reconciles to the server value when eventBridge invalidates the query', async () => {
    // Simulate the eventBridge path: server pushes `overlay.set`, the
    // bridge calls `invalidateQueries({ queryKey: ['overlay', ...] })`,
    // refetch returns the updated payload.
    const v1: LayoutValue = { positions: { a: { x: 0, y: 0, w: 1, h: 1 } } };
    const v2: LayoutValue = { positions: { a: { x: 0, y: 0, w: 2, h: 2 } } };
    const calls: LayoutValue[] = [v1, v2];
    (api.listOverlays as ReturnType<typeof vi.fn>).mockImplementation(() => {
      const v = calls.shift() ?? v2;
      return Promise.resolve([makeOverlay(v)]);
    });

    const client = makeClient();
    const { result } = renderHook(
      () =>
        useOverlayState<LayoutValue>({
          entity_kind: 'view',
          entity_id: 'w1',
          kind: 'layout',
          default: DEFAULT,
        }),
      { wrapper: wrapper(client) },
    );
    await waitFor(() => expect(result.current[0]).toEqual(v1));

    await act(async () => {
      await client.invalidateQueries({
        queryKey: overlayStateQueryKey('kernel', 'view', 'w1', 'layout'),
      });
    });
    await waitFor(() => expect(result.current[0]).toEqual(v2));
  });
});
