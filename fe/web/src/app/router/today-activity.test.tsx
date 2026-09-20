// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';

import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { DATABASE_ID_KEY } from '../../../../core/keys/storage.ts';
import { createUiPreferences } from '../providers/ui-preferences.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

const areas = [{ id: 'c1', name: 'One', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
const trackWire = (id: string, title: string) => ({
  id, area_id: 'c1', title, sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1_000,
});
const activityOverlay = (trackId: string, payload: Record<string, unknown>) => ({
  id: `activity-${trackId}`, plugin_id: 'kernel', entity_kind: 'track', entity_id: trackId, kind: 'activity',
  payload: { schemaVersion: 1, working: false, attention: 'none', activity_at_ms: null, items: [], cards: [], ...payload },
  updated_at: 1,
});

function renderToday() {
  const transport: ApiTransportPort = {
    send: (request) => {
      if (request.path === '/api/today/launchpad') return Promise.resolve({ status: 200, statusText: 'OK', body: null });
      if (request.path === '/api/areas') return Promise.resolve(ok(areas));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([
        trackWire('w-fresh', 'Fresh result'), trackWire('w-seen', 'Seen result'), trackWire('w-busy', 'Still busy'),
      ]));
      if (request.path.startsWith('/api/overlays?')) return Promise.resolve(ok([
        // Completed after the device's baseline: unread until this device looks.
        activityOverlay('w-fresh', { activity_at_ms: 150 }),
        // Completed before the baseline: read on arrival.
        activityOverlay('w-seen', { activity_at_ms: 50 }),
        // In flight, and with an unread completion behind it: motion wins.
        activityOverlay('w-busy', { working: true, activity_at_ms: 150 }),
      ]));
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  // A device that has entered this database's scope once, at server time 100.
  const values = new Map<string, string>([[DATABASE_ID_KEY, 'db1']]);
  const uiPreferences = createUiPreferences({ getItem: (key) => values.get(key) ?? null, setItem: (key, value) => { values.set(key, value); } });
  uiPreferences.setReadScope('db1', 100);
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined, uiPreferences });
  router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
}

afterEach(cleanup);

const markerOf = (root: HTMLElement, title: string) => within(root).getByRole('button', { name: new RegExp(`^Track ${title}`) })
  .parentElement?.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity') ?? null;

it('Today rows carry the track read receipt', async () => {
  renderToday();
  const main = await screen.findByRole('main');
  const open = (await within(main).findByRole('heading', { name: 'Open' })).closest('section')!;
  await waitFor(() => expect(markerOf(open, 'Fresh result')).toBe('unread'));
  expect(markerOf(open, 'Seen result')).toBeNull();
  expect(markerOf(open, 'Still busy')).toBe('working');

  // The same receipt key as the rail: both surfaces answer alike for one track.
  const rail = screen.getByRole('navigation', { name: 'Workspace' });
  expect(markerOf(rail, 'Fresh result')).toBe('unread');
  expect(markerOf(rail, 'Seen result')).toBeNull();
  expect(markerOf(rail, 'Still busy')).toBe('working');
});

/* Three tracks in the `working` phase, one of them with the kernel's `working` verdict: the
 * header's second number is that verdict's count, not the phase's. */
it('Today\'s second number counts the kernel\'s working verdict, not the running phase', async () => {
  renderToday();
  const main = await screen.findByRole('main');
  await within(main).findByRole('heading', { name: 'Open' });
  await waitFor(() => expect(within(main).getByRole('banner').textContent).toContain('1working'));
  expect(within(main).getByRole('banner').textContent).not.toMatch(/3working|in progress/);
});
