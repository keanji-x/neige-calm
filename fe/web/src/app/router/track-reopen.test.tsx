// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { TrackWire } from '../../../../core/domain/track.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

afterEach(cleanup);

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const area = {
  id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1,
};

type Harness = Readonly<{ requests: ApiRequest[] }>;

/** The whole router over a fake kernel whose PATCH flips `closed_at` and whose detail refetch then fails. */
function renderTrack(closedAt: number | null): Harness {
  const requests: ApiRequest[] = [];
  let track: TrackWire = {
    id: 'w1', area_id: 'c1', title: 'Recover me', sort: 1, cwd: '/tmp',
    pinned_at: null, closed_at: closedAt, created_at: 1, updated_at: 2,
  };
  let patchCommitted = false;
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = {
    send(request) {
      requests.push(request);
      if (request.path === '/api/areas') return Promise.resolve(ok([area]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([track]));
      if (request.path === '/api/overlays?entity_kind=track') return Promise.resolve(ok([]));
      if (request.method === 'PATCH' && request.path === '/api/tracks/w1') {
        const { closed } = request.body as { closed: boolean };
        track = { ...track, closed_at: closed ? 42 : null, updated_at: track.updated_at + 1 };
        patchCommitted = true;
        return Promise.resolve(ok(track));
      }
      if (request.path === '/api/tracks/w1') {
        if (patchCommitted) {
          return Promise.resolve({ status: 500, statusText: 'Refresh failed', body: {} });
        }
        const closed = track.closed_at !== null;
        return Promise.resolve(ok({ track, can_reopen: closed, can_close: !closed, cards: [], overlays: [] }));
      }
      if (request.path.endsWith('/conversations')) return Promise.resolve(ok([]));
      if (request.path === '/api/settings') return Promise.resolve(ok({}));
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn(),
  });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });

  render(
    <QueryClientProvider client={client}>
      <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  );
  return { requests };
}

function patches(requests: readonly ApiRequest[]): ApiRequest[] {
  return requests.filter((request) => request.method === 'PATCH' && request.path === '/api/tracks/w1');
}

it('PATCHes closed: false and drops the Closed badge after Reopen', async () => {
  const { requests } = renderTrack(42);

  expect(await screen.findAllByRole('status', { name: 'Track closed' })).toHaveLength(2);
  await userEvent.click(await screen.findByRole('button', { name: 'Track actions for Recover me' }));
  expect(screen.queryByRole('menuitem', { name: 'Close' })).toBeNull();
  await userEvent.click(screen.getByRole('menuitem', { name: /Reopen/ }));

  await waitFor(() => {
    expect(patches(requests)).toEqual([expect.objectContaining({ body: { closed: false } })]);
  });
  await waitFor(() => expect(screen.queryAllByRole('status', { name: 'Track closed' })).toHaveLength(0));
  await userEvent.click(screen.getByRole('button', { name: 'Track actions for Recover me' }));
  expect(screen.queryByRole('menuitem', { name: 'Reopen' })).toBeNull();
  expect(screen.getByRole('menuitem', { name: 'Delete track' })).toBeTruthy();
});

it('PATCHes closed: true and shows the Closed badge after Close', async () => {
  const { requests } = renderTrack(null);

  await userEvent.click(await screen.findByRole('button', { name: 'Track actions for Recover me' }));
  expect(screen.queryAllByRole('status', { name: 'Track closed' })).toHaveLength(0);
  expect(screen.queryByRole('menuitem', { name: /Reopen/ })).toBeNull();
  await userEvent.click(screen.getByRole('menuitem', { name: 'Close' }));

  await waitFor(() => {
    expect(patches(requests)).toEqual([expect.objectContaining({ body: { closed: true } })]);
  });
  expect(await screen.findAllByRole('status', { name: 'Track closed' })).toHaveLength(2);
  await userEvent.click(screen.getByRole('button', { name: 'Track actions for Recover me' }));
  expect(screen.queryByRole('menuitem', { name: 'Close' })).toBeNull();
  expect(screen.getByRole('menuitem', { name: 'Delete track' })).toBeTruthy();
});
