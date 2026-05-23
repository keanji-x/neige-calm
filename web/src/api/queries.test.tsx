// Unit tests for the TanStack Query factories + hooks in `queries.ts`.
//
// We mock the calm.ts REST client wholesale — every query/mutation here
// ultimately calls one of those functions, so swapping them with vi.fn()
// stubs lets us assert hook behavior without a server. Per
// `tests/setup.ts`, expect/describe/it are globals; we import the rest.

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

// Hoisted mock for the api client. Each function returns a Promise stub so
// React Query treats them as proper async resolutions.
vi.mock('./calm', () => ({
  listCoves: vi.fn(),
  wavesInCove: vi.fn(),
  getWaveDetail: vi.fn(),
  createCove: vi.fn(),
  updateCove: vi.fn(),
  deleteCove: vi.fn(),
  createWave: vi.fn(),
  updateWave: vi.fn(),
  deleteWave: vi.fn(),
  createCard: vi.fn(),
  updateCard: vi.fn(),
  deleteCard: vi.fn(),
}));

import * as api from './calm';
import {
  covesQueryOptions,
  wavesByCoveQueryOptions,
  waveDetailQueryOptions,
  queryKeys,
  useCovesQuery,
  useWaveDetailQuery,
  useCreateCoveMutation,
} from './queries';

// --- helpers -----------------------------------------------------------

/** Fresh QueryClient per test — Query caches between renders, and we don't
 *  want one test's `listCoves` resolution leaking into the next. Turn off
 *  retries so a deliberately-rejecting mutation/test errors fast.            */
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

beforeEach(() => {
  vi.clearAllMocks();
});

// --- query key factories -----------------------------------------------

describe('queryKeys / query option factories', () => {
  it("covesQueryOptions uses ['coves'] as queryKey", () => {
    const opts = covesQueryOptions();
    expect(opts.queryKey).toEqual(['coves']);
    expect(typeof opts.queryFn).toBe('function');
  });

  it('wavesByCoveQueryOptions interpolates coveId', () => {
    const opts = wavesByCoveQueryOptions('cove_xyz');
    expect(opts.queryKey).toEqual(['waves', 'cove_xyz']);
  });

  it('waveDetailQueryOptions interpolates waveId', () => {
    const opts = waveDetailQueryOptions('wave_abc');
    expect(opts.queryKey).toEqual(['wave', 'wave_abc']);
  });

  it('queryKeys helpers match the factory output', () => {
    expect(queryKeys.coves()).toEqual(['coves']);
    expect(queryKeys.wavesInCove('c1')).toEqual(['waves', 'c1']);
    expect(queryKeys.waveDetail('w1')).toEqual(['wave', 'w1']);
  });
});

// --- hooks --------------------------------------------------------------

describe('useCovesQuery', () => {
  it('starts in pending state and resolves to the mocked list', async () => {
    const fakeCoves = [
      {
        id: 'c1',
        name: 'Atlas',
        color: '#abc',
        sort: 0,
        kind: 'user' as const,
        created_at: 1,
        updated_at: 2,
      },
    ];
    (api.listCoves as ReturnType<typeof vi.fn>).mockResolvedValue(fakeCoves);

    const client = makeClient();
    const { result } = renderHook(() => useCovesQuery(), {
      wrapper: wrapper(client),
    });

    // Initial render: data not in cache yet. Either `isPending` or
    // `isLoading` is true depending on RQ version — checking `data` is the
    // version-agnostic signal.
    expect(result.current.data).toBeUndefined();

    await waitFor(() => expect(result.current.data).toEqual(fakeCoves));
    expect(api.listCoves).toHaveBeenCalledTimes(1);
  });
});

describe('useWaveDetailQuery', () => {
  it('stays disabled (no fetch) when waveId is null', () => {
    const client = makeClient();
    renderHook(() => useWaveDetailQuery(null), { wrapper: wrapper(client) });
    expect(api.getWaveDetail).not.toHaveBeenCalled();
  });

  it('fires the fetch when waveId is provided', async () => {
    (api.getWaveDetail as ReturnType<typeof vi.fn>).mockResolvedValue({
      wave: {
        id: 'w1',
        cove_id: 'c1',
        title: 't',
        sort: 0,
        archived_at: null,
        created_at: 1,
        updated_at: 2,
      },
      cards: [],
      overlays: [],
    });
    const client = makeClient();
    const { result } = renderHook(() => useWaveDetailQuery('w1'), {
      wrapper: wrapper(client),
    });
    await waitFor(() => expect(result.current.data).toBeDefined());
    expect(api.getWaveDetail).toHaveBeenCalledWith('w1');
  });

  // #177 regression: this is the load-bearing anchor for the entire bug
  // chain. `WaveComponent` early-returns `null` when `!detailQ.data`,
  // which unmounts the XtermView subtree and wipes its `prevThemeRef` —
  // so the next theme toggle's `TerminalThemeUpdate` OSC never fires.
  //
  // The fix is `placeholderData: keepPreviousData` on the wave detail
  // query. The clearest behavioral anchor is the "query key switch"
  // case: when the hook's `waveId` changes from w1→w2 while w2's fetch
  // is in flight, `data` STILL holds the previous wave (w1) instead of
  // briefly going `undefined`. Without `placeholderData`, the gap is
  // visible to consumers and triggers the early-return + unmount chain.
  //
  // This test would FAIL if `placeholderData: keepPreviousData` were
  // removed from `useWaveDetailQuery` — verified locally by commenting
  // out the line and re-running.
  it('keeps previous data visible across a waveId switch (#177)', async () => {
    const waveA = {
      wave: {
        id: 'w1',
        cove_id: 'c1',
        title: 'A',
        sort: 0,
        archived_at: null,
        created_at: 1,
        updated_at: 2,
      },
      cards: [],
      overlays: [],
    };
    const waveB = {
      wave: {
        id: 'w2',
        cove_id: 'c1',
        title: 'B',
        sort: 1,
        archived_at: null,
        created_at: 3,
        updated_at: 4,
      },
      cards: [],
      overlays: [],
    };

    // Gated mock: w1 resolves immediately, w2 hangs on a manually-
    // released promise so we can inspect the hook state during the
    // cross-key transition window.
    let releaseB!: (value: typeof waveB) => void;
    const bPending = new Promise<typeof waveB>((resolve) => {
      releaseB = resolve;
    });
    (api.getWaveDetail as ReturnType<typeof vi.fn>).mockImplementation(
      (id: string) =>
        id === 'w1' ? Promise.resolve(waveA) : bPending,
    );

    // Use a client without gcTime:0 so the observer doesn't get torn down
    // between key switches; production keeps queries cached across nav.
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    const { result, rerender } = renderHook(
      ({ id }: { id: string }) => useWaveDetailQuery(id),
      {
        wrapper: wrapper(client),
        initialProps: { id: 'w1' },
      },
    );

    // Initial fetch resolves to waveA.
    await waitFor(() => expect(result.current.data).toEqual(waveA));

    // Switch the hook's waveId to w2. The cache has no entry for w2 yet
    // and the mock for w2 is gated. WITHOUT `keepPreviousData`, the
    // hook's `data` would be `undefined` in this transition window.
    rerender({ id: 'w2' });

    // The critical anchor: data is the previous wave (waveA), NOT
    // undefined. WaveComponent's `if (!detailQ.data)` guard reads
    // truthy here, so the subtree stays mounted.
    expect(result.current.data).toEqual(waveA);
    expect(result.current.data).toBeTruthy();
    // RQ flags this explicitly as the placeholder window.
    expect(result.current.isPlaceholderData).toBe(true);

    // Release w2's fetch — data transitions to waveB.
    releaseB(waveB);
    await waitFor(() => expect(result.current.data).toEqual(waveB));
    expect(result.current.isPlaceholderData).toBe(false);
  });

  // #177 follow-on: `WaveComponent` (router.tsx) early-returns `null` on
  // `!detailQ.data`, which would unmount the lazy XtermView subtree mid-
  // transition. We record `detailQ.data` truthiness on EVERY render
  // across the waveId switch and assert it never flips to falsy after
  // the first resolve — the exact guard the route component depends on.
  it('detailQ.data stays truthy on every render across waveId switch (#177 guard)', async () => {
    const waveA = {
      wave: {
        id: 'wa',
        cove_id: 'c1',
        title: 'A',
        sort: 0,
        archived_at: null,
        created_at: 1,
        updated_at: 2,
      },
      cards: [],
      overlays: [],
    };
    const waveB = {
      ...waveA,
      wave: { ...waveA.wave, id: 'wb', title: 'B' },
    };
    let releaseB!: (v: typeof waveB) => void;
    const bPending = new Promise<typeof waveB>((r) => {
      releaseB = r;
    });
    (api.getWaveDetail as ReturnType<typeof vi.fn>).mockImplementation(
      (id: string) => (id === 'wa' ? Promise.resolve(waveA) : bPending),
    );

    const client = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    const renders: { id: string; data: unknown }[] = [];
    const { result, rerender } = renderHook(
      ({ id }: { id: string }) => {
        const q = useWaveDetailQuery(id);
        renders.push({ id, data: q.data });
        return q;
      },
      { wrapper: wrapper(client), initialProps: { id: 'wa' } },
    );

    await waitFor(() => expect(result.current.data).toEqual(waveA));
    rerender({ id: 'wb' });
    // Don't release B yet — let the placeholder window be observable.
    // One more synchronous tick to flush.
    await Promise.resolve();
    releaseB(waveB);
    await waitFor(() => expect(result.current.data).toEqual(waveB));

    // After the FIRST render that surfaced data (the initial waveA
    // resolve), no later render may drop back to falsy. The mid-switch
    // render with id='wb' is the bug window — placeholderData makes it
    // hold waveA there too.
    const firstResolvedIdx = renders.findIndex((r) => !!r.data);
    expect(firstResolvedIdx).toBeGreaterThanOrEqual(0);
    const post = renders.slice(firstResolvedIdx);
    expect(post.length).toBeGreaterThan(1);
    // Confirm at least one render in `post` was for the switched id —
    // i.e. we actually exercised the cross-key transition.
    expect(post.some((r) => r.id === 'wb')).toBe(true);
    // The critical anchor: no render in the post-resolve series drops data.
    for (const r of post) {
      expect(r.data).toBeTruthy();
    }
  });
});

// --- mutations ----------------------------------------------------------

describe('useCreateCoveMutation', () => {
  it('calls api.createCove and invalidates the coves query on success', async () => {
    const newCove = {
      id: 'c2',
      name: 'New',
      color: '#fff',
      sort: 1,
      created_at: 1,
      updated_at: 2,
    };
    (api.createCove as ReturnType<typeof vi.fn>).mockResolvedValue(newCove);

    const client = makeClient();
    const invalidateSpy = vi.spyOn(client, 'invalidateQueries');

    const { result } = renderHook(() => useCreateCoveMutation(), {
      wrapper: wrapper(client),
    });

    await result.current.mutateAsync({ name: 'New', color: '#fff' });

    expect(api.createCove).toHaveBeenCalledWith({ name: 'New', color: '#fff' });
    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: ['coves'] });
  });
});
