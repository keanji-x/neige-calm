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

  it('keeps previous data visible across an invalidate-driven refetch (#177)', async () => {
    // Regression guard for the `placeholderData: keepPreviousData` flip.
    // Without it, an `invalidateQueries({ queryKey: ['wave', 'w1'] })`
    // call would briefly surface `data: undefined` for the duration of
    // the refetch — exactly the "subtree unmount" trigger that wiped
    // XtermView's `pendingThemeRef` / `sendRef` and dropped the in-
    // flight theme dispatch in the #177 bug chain.
    const firstSnapshot = {
      wave: {
        id: 'w1',
        cove_id: 'c1',
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
      wave: { ...firstSnapshot.wave, title: 'second', updated_at: 3 },
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
    (api.getWaveDetail as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce(firstSnapshot)
      .mockReturnValueOnce(secondPromise);

    const client = makeClient();
    const { result } = renderHook(() => useWaveDetailQuery('w1'), {
      wrapper: wrapper(client),
    });

    await waitFor(() =>
      expect(result.current.data?.wave.title).toBe('first'),
    );

    // Kick off the refetch via invalidate. The returned promise only
    // resolves after the refetch settles, so we don't await it here.
    // Instead, we poll `result.current.data` to confirm it stays
    // defined across the refetch window — that's the core invariant
    // `placeholderData: keepPreviousData` provides.
    const invalidated = client.invalidateQueries({
      queryKey: queryKeys.waveDetail('w1'),
    });

    // Poll a few ticks; if `placeholderData` is missing, `data` would
    // briefly flip to `undefined`. Polling avoids racing the initial
    // refetch microtask.
    for (let i = 0; i < 5; i += 1) {
      await new Promise((r) => setTimeout(r, 10));
      expect(result.current.data).toBeDefined();
      expect(result.current.data!.wave.title).toBe('first');
    }

    // Release the second resolution; the hook should swap to the new data.
    releaseSecond(secondSnapshot);
    await invalidated;
    await waitFor(() =>
      expect(result.current.data?.wave.title).toBe('second'),
    );
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
