// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

afterEach(cleanup);
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

it('requires the shared confirmation before deleting a Today panel track', async () => {
  const requests: ApiRequest[] = [];
  let deleted = false;
  const track = {
    id: 'w1', area_id: 'c1', title: 'Risky', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: Date.now() - 1000, updated_at: Date.now(),
  };
  const transport: ApiTransportPort = { send(request): Promise<ApiTransportResponse> {
    requests.push(request);
    if (request.path === '/api/areas') return Promise.resolve({ status: 200, statusText: 'OK', body: [
      { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
    ] });
    if (request.path === '/api/areas/c1/tracks') return Promise.resolve({ status: 200, statusText: 'OK', body: deleted ? [] : [track] });
    if (request.method === 'DELETE') deleted = true;
    return Promise.resolve({ status: 200, statusText: 'OK', body: request.method === 'DELETE' ? undefined : [] });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);

  await screen.findByRole('complementary');
  await userEvent.click(await within(screen.getByRole('complementary')).findByRole('button', { name: 'Delete Risky' }));
  expect(screen.getByRole('dialog', { name: 'Delete this track?' })).toBeTruthy();
  expect(requests.filter((request) => request.method === 'DELETE')).toHaveLength(0);
  await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));
  expect(requests.filter((request) => request.method === 'DELETE')).toHaveLength(1);
  await waitFor(() => expect(screen.queryByRole('button', { name: 'Delete Risky' })).toBeNull());
  expect(document.activeElement).toBe(document.querySelector('[data-nc-page-title]'));
});

it('does not navigate on a delete success that arrives after cancellation', async () => {
  let resolveDelete!: (response: ApiTransportResponse) => void;
  const track = { id: 'w1', area_id: 'c1', title: 'Risky', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1 };
  const transport: ApiTransportPort = { send(request): Promise<ApiTransportResponse> {
    if (request.method === 'DELETE') return new Promise((resolve) => { resolveDelete = resolve; });
    if (request.path === '/api/areas') return Promise.resolve({ status: 200, statusText: 'OK', body: [
      { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
    ] });
    if (request.path === '/api/areas/c1/tracks') return Promise.resolve({ status: 200, statusText: 'OK', body: [track] });
    if (request.path === '/api/tracks/w1') return Promise.resolve({ status: 200, statusText: 'OK', body: { track, cards: [], overlays: [] } });
    if (request.path === '/api/settings') return Promise.resolve({ status: 200, statusText: 'OK', body: {} });
    return Promise.resolve({ status: 200, statusText: 'OK', body: [] });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  await userEvent.click(await screen.findByRole('button', { name: 'Delete track Risky' }));
  await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));
  await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
  await userEvent.click(screen.getByRole('button', { name: 'Account menu for You' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'Settings' }));
  resolveDelete({ status: 204, statusText: 'No Content', body: undefined });
  await waitFor(() => expect(router.state.location.pathname).toBe('/settings'));
});

it('does not navigate on an area delete success that arrives after cancellation', async () => {
  let resolveDelete!: (response: ApiTransportResponse) => void;
  const transport: ApiTransportPort = { send(request): Promise<ApiTransportResponse> {
    if (request.method === 'DELETE') return new Promise((resolve) => { resolveDelete = resolve; });
    if (request.path === '/api/areas') return Promise.resolve({ status: 200, statusText: 'OK', body: [
      { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
    ] });
    if (request.path === '/api/areas/c1/tracks') return Promise.resolve({ status: 200, statusText: 'OK', body: [] });
    return Promise.resolve({ status: 200, statusText: 'OK', body: [] });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  await userEvent.click(await screen.findByRole('button', { name: 'Area actions for Work' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'Delete area' }));
  await userEvent.type(screen.getByRole('textbox', { name: 'Type Work to confirm.' }), 'Work');
  await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));
  await waitFor(() => expect(resolveDelete).toBeTypeOf('function'));
  await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
  resolveDelete({ status: 204, statusText: 'No Content', body: undefined });
  await new Promise((done) => { setTimeout(done, 10); });
  expect(router.state.location.pathname).toBe('/');
});

it('round-trips an encoded track id through useGo, TanStack history, and useRouteParam', async () => {
  const requests: ApiRequest[] = [];
  const trackId = 'a/b %';
  const track = { id: trackId, area_id: 'c1', title: 'Encoded track', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1 };
  const transport: ApiTransportPort = { send(request): Promise<ApiTransportResponse> {
    requests.push(request);
    if (request.path === '/api/areas') return Promise.resolve({ status: 200, statusText: 'OK', body: [
      { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
    ] });
    if (request.path === '/api/areas/c1/tracks') return Promise.resolve({ status: 200, statusText: 'OK', body: [track] });
    if (request.path.includes('/api/tracks/')) return Promise.resolve({ status: 200, statusText: 'OK', body: { track, cards: [], overlays: [] } });
    return Promise.resolve({ status: 200, statusText: 'OK', body: [] });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  const rail = await screen.findByRole('navigation', { name: 'Workspace' });
  await userEvent.click(await within(rail).findByText('Encoded track'));
  // TanStack normalises %20 back to a space in memory history while retaining
  // the escapes that delimit the segment; useRouteParam still restores all of it.
  expect(router.state.location.pathname).toBe('/track/a%2Fb %25');
  expect(within(rail).getByRole('button', { name: /Track Encoded track/ }).getAttribute('aria-current')).toBe('page');
  await screen.findByRole('button', { name: 'Rename track' });
  expect(requests.some((request) => request.path.includes('a%2Fb%20%25'))).toBe(true);
});
