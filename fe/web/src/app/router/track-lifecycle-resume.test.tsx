// @vitest-environment jsdom
//
// #1450 — the production seam from the Track header through app/router and the
// shared mutation port. Component tests own the menu itself; this file proves
// the selected action becomes the existing PATCH request and the returned row
// replaces Done with Working on screen.

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

it('PATCHes Working and renders the returned lifecycle after Resume work', async () => {
  const requests: ApiRequest[] = [];
  let track: TrackWire = {
    id: 'w1', area_id: 'c1', title: 'Recover me', sort: 1, lifecycle: 'done', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: 42, created_at: 1, updated_at: 2,
  };
  let canResume = true;
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = {
    send(request) {
      requests.push(request);
      if (request.path === '/api/areas') return Promise.resolve(ok([area]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([track]));
      if (request.path === '/api/overlays?entity_kind=track') return Promise.resolve(ok([]));
      if (request.method === 'PATCH' && request.path === '/api/tracks/w1') {
        track = {
          ...track,
          ...(request.body as Partial<typeof track>),
          terminal_at: null,
          updated_at: track.updated_at + 1,
        };
        canResume = false;
        return Promise.resolve(ok(track));
      }
      if (request.path === '/api/tracks/w1') {
        return Promise.resolve(ok({ track, can_resume: canResume, cards: [], overlays: [] }));
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

  await userEvent.click(await screen.findByRole('button', { name: 'Track lifecycle: Done' }));
  await userEvent.click(screen.getByRole('menuitem', { name: /Resume work/ }));

  await waitFor(() => {
    expect(requests.filter((request) => request.method === 'PATCH' && request.path === '/api/tracks/w1'))
      .toEqual([expect.objectContaining({ body: { lifecycle: 'working' } })]);
  });
  await screen.findByRole('button', { name: 'Track lifecycle: Working' });
});
