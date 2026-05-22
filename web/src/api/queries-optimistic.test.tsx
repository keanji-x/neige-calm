// Unit tests for optimistic-update + rollback behavior on the cove update
// mutation (`useUpdateCoveMutation`). Counterpart to `queries.test.tsx`,
// which covers the "happy path" hook surface.
//
// Why this lives in its own file:
//   - The mutation lifecycle here is non-trivial (`onMutate` writes
//     optimistic state, `onError` rolls back, `onSettled` invalidates).
//     Asserting all three in a single render with timing-sensitive
//     intermediate cache reads is verbose enough to deserve its own scope.
//   - It pins the rollback contract: the slice that landed optimistic
//     renames (Wave 3.L) is easy to silently break by reshaping the
//     `previous` snapshot or forgetting the `onError` arm.
//
// Pattern (matches TanStack Query's own optimistic tests):
//   - Seed cache with [{ id: 'c1', name: 'before' }]
//   - Make api.updateCove reject (deferred so we can read mid-flight state)
//   - Call mutate({ id, body: { name: 'after' } })
//   - Verify mid-flight: cache shows 'after' (optimistic write applied)
//   - Resolve the rejection
//   - Verify post-error: cache rolled back to 'before'

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

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
import { useUpdateCoveMutation, queryKeys } from './queries';
import type { KernelCove } from './wire';

function makeClient(): QueryClient {
  // `gcTime: 0` would immediately garbage-collect `setQueryData` writes that
  // have no live observers, which means the cache lookup inside `onMutate`
  // would see undefined and skip the optimistic write. Keep gc generous —
  // we make a fresh client per test so cross-test leakage isn't a risk.
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: Infinity, staleTime: Infinity },
      mutations: { retry: false },
    },
  });
}

function wrap(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

function seedCove(client: QueryClient, name: string): KernelCove {
  const cove: KernelCove = {
    id: 'c1',
    name,
    color: '#abc',
    sort: 0,
    kind: 'user',
    created_at: 1,
    updated_at: 2,
  };
  client.setQueryData(queryKeys.coves(), [cove]);
  return cove;
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe('useUpdateCoveMutation — optimistic update + rollback', () => {
  it('applies the optimistic name mid-flight then rolls back on rejection', async () => {
    const client = makeClient();
    seedCove(client, 'before');

    // The mutationFn rejects, so onError fires and the rollback should
    // restore the snapshot. We hold the rejection in a Promise we control,
    // so we can read the cache between onMutate and onError.
    let rejectFn: ((err: Error) => void) | null = null;
    const pending = new Promise<never>((_resolve, reject) => {
      rejectFn = reject;
    });
    (api.updateCove as ReturnType<typeof vi.fn>).mockReturnValue(pending);

    const { result } = renderHook(() => useUpdateCoveMutation(), {
      wrapper: wrap(client),
    });

    // Kick the mutation. We deliberately don't await — we want to inspect
    // mid-flight state. `.catch` swallows the eventual rejection so vitest
    // doesn't complain about an unhandled rejection from this Promise.
    act(() => {
      result.current
        .mutate({ id: 'c1', body: { name: 'after' } });
    });

    // onMutate runs synchronously-ish; the optimistic write should be
    // visible on the cache before the mutationFn settles.
    await waitFor(() => {
      const cached = client.getQueryData<KernelCove[]>(queryKeys.coves());
      expect(cached?.[0]?.name).toBe('after');
    });

    // Now resolve the rejection — onError + onSettled fire.
    expect(rejectFn).not.toBeNull();
    await act(async () => {
      rejectFn!(new Error('boom'));
      // Let the microtask queue drain so onError + onSettled both run.
      await Promise.resolve();
      await Promise.resolve();
    });

    // Rollback restored the snapshot. onSettled also invalidates, but with
    // no real fetcher (mock returns nothing useful) the cache value sticks
    // at the rolled-back snapshot.
    await waitFor(() => {
      const cached = client.getQueryData<KernelCove[]>(queryKeys.coves());
      expect(cached?.[0]?.name).toBe('before');
    });

    expect(result.current.isError).toBe(true);
  });

  it('non-optimistic patch fields (e.g. sort) skip the snapshot path', async () => {
    // Reorder patches don't take the optimistic path — onMutate returns
    // `{ previous: null }`, so onError has nothing to restore. This test
    // pins that behavior so future "should we make this optimistic too?"
    // changes are visible in the diff.
    const client = makeClient();
    seedCove(client, 'unchanged');

    (api.updateCove as ReturnType<typeof vi.fn>).mockRejectedValueOnce(new Error('boom'));

    const { result } = renderHook(() => useUpdateCoveMutation(), {
      wrapper: wrap(client),
    });

    await act(async () => {
      await result.current
        .mutateAsync({ id: 'c1', body: { sort: 5 } })
        .catch(() => {});
    });

    // Cache never moved because we never wrote an optimistic copy.
    const cached = client.getQueryData<KernelCove[]>(queryKeys.coves());
    expect(cached?.[0]?.name).toBe('unchanged');
    expect(cached?.[0]?.sort).toBe(0);
    // Mutation state updates asynchronously after the mutationFn settles —
    // wait for the renderHook to surface the error flag.
    await waitFor(() => expect(result.current.isError).toBe(true));
  });
});
