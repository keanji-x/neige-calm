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
