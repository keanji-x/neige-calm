// Unit tests for `useCardOverlay` — the REST-seeded card overlay reader.
//
// The load-bearing case (#854 / PR #867 review finding 1): the hook must
// yield the correct overlay payload from the REST snapshot alone, with NO
// `overlay.set` stream frame ever arriving. That is exactly the shape of
// an over-cap cold connect, where the server skips the replay backlog and
// goes straight to `_replay_complete` — the old stream-fold implementation
// stayed `null` there and every live card dot regressed to "Starting".
//
// Live convergence is invalidation-driven (eventBridge invalidates
// `['overlays', 'card']` on overlay events and runs a global invalidate on
// `_replay_complete`); we simulate those invalidations directly against
// the QueryClient — the bridge's own dispatch is covered by
// `eventBridge.test.tsx`.

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

vi.mock('../api/calm', () => ({
  listAllOverlays: vi.fn(),
}));

import * as api from '../api/calm';
import { queryKeys } from '../api/queries';
import { useCardStatusOverlay } from './overlayRegistry';
import { useCardOverlay } from './useCardOverlay';
import type { KernelOverlay } from '../api/wire';

function makeOverlay(
  entity_id: string,
  kind: string,
  payload: unknown,
): KernelOverlay {
  return {
    id: `ov-${entity_id}-${kind}`,
    plugin_id: 'kernel',
    entity_kind: 'card',
    entity_id,
    kind,
    payload,
    updated_at: 0,
  };
}

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0 },
    },
  });
}

function wrapper(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

const listAllOverlays = api.listAllOverlays as ReturnType<typeof vi.fn>;

beforeEach(() => {
  vi.clearAllMocks();
});

describe('useCardOverlay', () => {
  it('seeds from REST with no stream frames — the over-cap cold-connect case', async () => {
    // No `overlay.set` frame is ever delivered in this test. The REST
    // snapshot alone must populate the value, or an over-cap cold connect
    // (backlog skipped, `_replay_complete` straight at the tip) leaves
    // every card overlay consumer stuck at null.
    listAllOverlays.mockResolvedValue([
      makeOverlay('card_other', 'status', { state: 'Ignored' }),
      makeOverlay('card_1', 'badge', { state: 'Ignored' }),
      makeOverlay('card_1', 'status', { state: 'Working' }),
    ]);

    const client = makeClient();
    const { result } = renderHook(
      () => useCardOverlay<{ state: string }>('card_1', 'status'),
      { wrapper: wrapper(client) },
    );

    expect(result.current).toBeNull(); // pending fetch
    await waitFor(() => {
      expect(result.current).toEqual({ state: 'Working' });
    });
    expect(listAllOverlays).toHaveBeenCalledWith('card');
  });

  it('returns null when no overlay matches (cardId, kind)', async () => {
    listAllOverlays.mockResolvedValue([
      makeOverlay('card_1', 'badge', { state: 'Ignored' }),
    ]);

    const client = makeClient();
    const { result } = renderHook(
      () => useCardOverlay<{ state: string }>('card_1', 'status'),
      { wrapper: wrapper(client) },
    );

    await waitFor(() => {
      expect(listAllOverlays).toHaveBeenCalled();
    });
    expect(result.current).toBeNull();
  });

  it('does not fetch without a cardId', async () => {
    listAllOverlays.mockResolvedValue([]);

    const client = makeClient();
    const { result } = renderHook(
      () => useCardOverlay<{ state: string }>(undefined, 'status'),
      { wrapper: wrapper(client) },
    );

    expect(result.current).toBeNull();
    // The query is disabled: give microtasks a beat, then assert no fetch.
    await Promise.resolve();
    expect(listAllOverlays).not.toHaveBeenCalled();
  });

  it('converges on the new value when the snapshot is invalidated (overlay.set path)', async () => {
    listAllOverlays.mockResolvedValue([
      makeOverlay('card_1', 'status', { state: 'Working' }),
    ]);

    const client = makeClient();
    const { result } = renderHook(
      () => useCardOverlay<{ state: string }>('card_1', 'status'),
      { wrapper: wrapper(client) },
    );
    await waitFor(() => {
      expect(result.current).toEqual({ state: 'Working' });
    });

    // eventBridge's `overlay.set` policy invalidates ['overlays','card'];
    // the refetch must land the new payload.
    listAllOverlays.mockResolvedValue([
      makeOverlay('card_1', 'status', { state: 'Ready' }),
    ]);
    await client.invalidateQueries({
      queryKey: queryKeys.overlaysByKind('card'),
    });
    await waitFor(() => {
      expect(result.current).toEqual({ state: 'Ready' });
    });
  });

  it('drops to null when the overlay disappears from the snapshot (overlay.deleted path)', async () => {
    listAllOverlays.mockResolvedValue([
      makeOverlay('card_1', 'status', { state: 'Working' }),
    ]);

    const client = makeClient();
    const { result } = renderHook(
      () => useCardOverlay<{ state: string }>('card_1', 'status'),
      { wrapper: wrapper(client) },
    );
    await waitFor(() => {
      expect(result.current).toEqual({ state: 'Working' });
    });

    listAllOverlays.mockResolvedValue([]);
    await client.invalidateQueries({
      queryKey: queryKeys.overlaysByKind('card'),
    });
    await waitFor(() => {
      expect(result.current).toBeNull();
    });
  });

  it('re-selects from the shared snapshot when cardId changes', async () => {
    listAllOverlays.mockResolvedValue([
      makeOverlay('A', 'status', { state: 'A payload' }),
      makeOverlay('B', 'status', { state: 'B payload' }),
    ]);

    const client = makeClient();
    const { result, rerender } = renderHook(
      ({ cardId }) => useCardOverlay<{ state: string }>(cardId, 'status'),
      { initialProps: { cardId: 'A' }, wrapper: wrapper(client) },
    );
    await waitFor(() => {
      expect(result.current).toEqual({ state: 'A payload' });
    });

    // Same shared cache entry — the new selection is available without a
    // second fetch.
    rerender({ cardId: 'B' });
    await waitFor(() => {
      expect(result.current).toEqual({ state: 'B payload' });
    });
    expect(listAllOverlays).toHaveBeenCalledTimes(1);
  });

  it('filters status overlays through useCardStatusOverlay', async () => {
    listAllOverlays.mockResolvedValue([
      makeOverlay('card_1', 'badge', { state: 'Ignored' }),
      makeOverlay('card_1', 'status', { state: 'Ready' }),
    ]);

    const client = makeClient();
    const { result } = renderHook(() => useCardStatusOverlay('card_1'), {
      wrapper: wrapper(client),
    });

    await waitFor(() => {
      expect(result.current).toEqual({ state: 'Ready' });
    });
  });
});
