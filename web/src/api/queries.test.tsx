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
import type { KernelArea } from './wire';

// Hoisted mock for the api client. Each function returns a Promise stub so
// React Query treats them as proper async resolutions.
vi.mock('./calm', () => ({
  listAreas: vi.fn(),
  tracksInArea: vi.fn(),
  getTrackDetail: vi.fn(),
  createArea: vi.fn(),
  updateArea: vi.fn(),
  deleteArea: vi.fn(),
  createTrack: vi.fn(),
  updateTrack: vi.fn(),
  deleteTrack: vi.fn(),
  createCard: vi.fn(),
  updateCard: vi.fn(),
  deleteCard: vi.fn(),
}));

import * as api from './calm';
import {
  areasQueryOptions,
  tracksByAreaQueryOptions,
  trackDetailQueryOptions,
  queryKeys,
  useAreasQuery,
  useTrackDetailQuery,
  useCreateAreaMutation,
} from './queries';

// --- helpers -----------------------------------------------------------

/** Fresh QueryClient per test — Query caches between renders, and we don't
 *  want one test's `listAreas` resolution leaking into the next. Turn off
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
  it("areasQueryOptions uses ['areas'] as queryKey", () => {
    const opts = areasQueryOptions();
    expect(opts.queryKey).toEqual(['areas']);
    expect(typeof opts.queryFn).toBe('function');
  });

  it('tracksByAreaQueryOptions interpolates areaId', () => {
    const opts = tracksByAreaQueryOptions('area_xyz');
    expect(opts.queryKey).toEqual(['tracks', 'area_xyz']);
  });

  it('trackDetailQueryOptions interpolates trackId', () => {
    const opts = trackDetailQueryOptions('track_abc');
    expect(opts.queryKey).toEqual(['track', 'track_abc']);
  });

  it('queryKeys helpers match the factory output', () => {
    expect(queryKeys.areas()).toEqual(['areas']);
    expect(queryKeys.tracksInArea('c1')).toEqual(['tracks', 'c1']);
    expect(queryKeys.trackDetail('w1')).toEqual(['track', 'w1']);
  });
});

// --- hooks --------------------------------------------------------------

describe('useAreasQuery', () => {
  it('starts in pending state and resolves to the mocked list', async () => {
    const fakeAreas: KernelArea[] = [
      {
        id: 'c1',
        name: 'Atlas',
        color: '#abc',
        sort: 0,
        kind: 'user',
        default_template_id: null,
        default_cwd: null,
        created_at: 1,
        updated_at: 2,
      },
    ];
    vi.mocked(api.listAreas).mockResolvedValue(fakeAreas);

    const client = makeClient();
    const { result } = renderHook(() => useAreasQuery(), {
      wrapper: wrapper(client),
    });

    // Initial render: data not in cache yet. Either `isPending` or
    // `isLoading` is true depending on RQ version — checking `data` is the
    // version-agnostic signal.
    expect(result.current.data).toBeUndefined();

    await waitFor(() => expect(result.current.data).toEqual(fakeAreas));
    expect(api.listAreas).toHaveBeenCalledTimes(1);
  });
});

describe('useTrackDetailQuery', () => {
  it('stays disabled (no fetch) when trackId is null', () => {
    const client = makeClient();
    renderHook(() => useTrackDetailQuery(null), { wrapper: wrapper(client) });
    expect(api.getTrackDetail).not.toHaveBeenCalled();
  });

  it('fires the fetch when trackId is provided', async () => {
    (api.getTrackDetail as ReturnType<typeof vi.fn>).mockResolvedValue({
      track: {
        id: 'w1',
        area_id: 'c1',
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
    const { result } = renderHook(() => useTrackDetailQuery('w1'), {
      wrapper: wrapper(client),
    });
    await waitFor(() => expect(result.current.data).toBeDefined());
    expect(api.getTrackDetail).toHaveBeenCalledWith('w1');
  });

  it('keeps previous data visible across an invalidate-driven refetch (#177)', async () => {
    // Regression guard for the `placeholderData: keepPreviousData` flip.
    // Without it, an `invalidateQueries({ queryKey: ['track', 'w1'] })`
    // call would briefly surface `data: undefined` for the duration of
    // the refetch — exactly the "subtree unmount" trigger that wiped
    // XtermView's `pendingThemeRef` / `sendRef` and dropped the in-
    // flight theme dispatch in the #177 bug chain.
    const firstSnapshot = {
      track: {
        id: 'w1',
        area_id: 'c1',
        title: 'first',
        sort: 0,
        archived_at: null,
        created_at: 1,
        updated_at: 2,
      },
      cards: [],
      overlays: [],
    };
    const secondSnapshot = {
      track: { ...firstSnapshot.track, title: 'second', updated_at: 3 },
      cards: [],
      overlays: [],
    };
    // Two resolutions: initial mount + post-invalidate refetch. Use a
    // delayed second promise so we can poll the hook state across the
    // refetch window before letting it settle.
    let releaseSecond!: (value: typeof secondSnapshot) => void;
    const secondPromise = new Promise<typeof secondSnapshot>((resolve) => {
      releaseSecond = resolve;
    });
    (api.getTrackDetail as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce(firstSnapshot)
      .mockReturnValueOnce(secondPromise);

    const client = makeClient();
    const { result } = renderHook(() => useTrackDetailQuery('w1'), {
      wrapper: wrapper(client),
    });

    await waitFor(() =>
      expect(result.current.data?.track.title).toBe('first'),
    );

    // Kick off the refetch via invalidate. The returned promise only
    // resolves after the refetch settles, so we don't await it here.
    // Instead, we poll `result.current.data` to confirm it stays
    // defined across the refetch window — that's the core invariant
    // `placeholderData: keepPreviousData` provides.
    const invalidated = client.invalidateQueries({
      queryKey: queryKeys.trackDetail('w1'),
    });

    // Poll a few ticks; if `placeholderData` is missing, `data` would
    // briefly flip to `undefined`. Polling avoids racing the initial
    // refetch microtask.
    for (let i = 0; i < 5; i += 1) {
      await new Promise((r) => setTimeout(r, 10));
      expect(result.current.data).toBeDefined();
      expect(result.current.data!.track.title).toBe('first');
    }

    // Release the second resolution; the hook should swap to the new data.
    releaseSecond(secondSnapshot);
    await invalidated;
    await waitFor(() =>
      expect(result.current.data?.track.title).toBe('second'),
    );
  });
});

// --- mutations ----------------------------------------------------------

describe('useCreateAreaMutation', () => {
  it('calls api.createArea and invalidates the areas query on success', async () => {
    const newArea: KernelArea = {
      id: 'c2',
      name: 'New',
      color: '#fff',
      sort: 1,
      kind: 'user',
      default_template_id: null,
      default_cwd: null,
      created_at: 1,
      updated_at: 2,
    };
    vi.mocked(api.createArea).mockResolvedValue(newArea);

    const client = makeClient();
    const invalidateSpy = vi.spyOn(client, 'invalidateQueries');

    const { result } = renderHook(() => useCreateAreaMutation(), {
      wrapper: wrapper(client),
    });

    await result.current.mutateAsync({ name: 'New', color: '#fff' });

    expect(api.createArea).toHaveBeenCalledWith({ name: 'New', color: '#fff' });
    expect(invalidateSpy).toHaveBeenCalledWith({ queryKey: ['areas'] });
  });
});
