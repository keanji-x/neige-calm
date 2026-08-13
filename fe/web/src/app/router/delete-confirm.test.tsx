// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';

afterEach(cleanup);

it('requires the shared confirmation before deleting a Today panel wave', async () => {
  const requests: ApiRequest[] = [];
  const wave = {
    id: 'w1', cove_id: 'c1', title: 'Risky', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: Date.now() - 1000, updated_at: Date.now(),
  };
  const transport: ApiTransportPort = { send(request): Promise<ApiTransportResponse> {
    requests.push(request);
    if (request.path === '/api/coves') return Promise.resolve({ status: 200, statusText: 'OK', body: [
      { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
    ] });
    if (request.path === '/api/coves/c1/waves') return Promise.resolve({ status: 200, statusText: 'OK', body: [wave] });
    return Promise.resolve({ status: 200, statusText: 'OK', body: request.method === 'DELETE' ? undefined : [] });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);

  await screen.findByRole('complementary');
  await userEvent.click(await within(screen.getByRole('complementary')).findByRole('button', { name: 'Delete Risky' }));
  expect(screen.getByRole('dialog', { name: 'Delete this wave?' })).toBeTruthy();
  expect(requests.filter((request) => request.method === 'DELETE')).toHaveLength(0);
  await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));
  expect(requests.filter((request) => request.method === 'DELETE')).toHaveLength(1);
});

it('requires the shared confirmation before deleting from the CoveRoute panel', async () => {
  const requests: ApiRequest[] = [];
  const wave = { id: 'w1', cove_id: 'c1', title: 'Risky', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1 };
  const transport: ApiTransportPort = { send(request): Promise<ApiTransportResponse> {
    requests.push(request);
    if (request.path === '/api/coves') return Promise.resolve({ status: 200, statusText: 'OK', body: [
      { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
    ] });
    if (request.path === '/api/coves/c1/waves') return Promise.resolve({ status: 200, statusText: 'OK', body: [wave] });
    return Promise.resolve({ status: 200, statusText: 'OK', body: request.method === 'DELETE' ? undefined : [] });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/cove/c1'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  await userEvent.click(await screen.findByRole('button', { name: 'Delete Risky' }));
  expect(requests.filter((request) => request.method === 'DELETE')).toHaveLength(0);
  await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));
  expect(requests.filter((request) => request.method === 'DELETE')).toHaveLength(1);
});

it('does not navigate on a delete success that arrives after cancellation', async () => {
  let resolveDelete!: (response: ApiTransportResponse) => void;
  const wave = { id: 'w1', cove_id: 'c1', title: 'Risky', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1 };
  const transport: ApiTransportPort = { send(request): Promise<ApiTransportResponse> {
    if (request.method === 'DELETE') return new Promise((resolve) => { resolveDelete = resolve; });
    if (request.path === '/api/coves') return Promise.resolve({ status: 200, statusText: 'OK', body: [
      { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
    ] });
    if (request.path === '/api/coves/c1/waves') return Promise.resolve({ status: 200, statusText: 'OK', body: [wave] });
    if (request.path === '/api/waves/w1') return Promise.resolve({ status: 200, statusText: 'OK', body: { wave, cards: [], overlays: [] } });
    if (request.path === '/api/settings') return Promise.resolve({ status: 200, statusText: 'OK', body: {} });
    return Promise.resolve({ status: 200, statusText: 'OK', body: [] });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/wave/w1'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  await userEvent.click(await screen.findByRole('button', { name: 'Delete wave Risky' }));
  await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));
  await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
  await userEvent.click(screen.getByRole('button', { name: 'Account menu for You' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'Settings' }));
  resolveDelete({ status: 204, statusText: 'No Content', body: undefined });
  await screen.findByRole('heading', { name: 'Settings' });
  expect(router.state.location.pathname).toBe('/settings');
});

it('round-trips an encoded wave id through useGo, TanStack history, and useRouteParam', async () => {
  const requests: ApiRequest[] = [];
  const waveId = 'a/b %';
  const wave = { id: waveId, cove_id: 'c1', title: 'Encoded wave', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1 };
  const transport: ApiTransportPort = { send(request): Promise<ApiTransportResponse> {
    requests.push(request);
    if (request.path === '/api/coves') return Promise.resolve({ status: 200, statusText: 'OK', body: [
      { id: 'c1', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
    ] });
    if (request.path === '/api/coves/c1/waves') return Promise.resolve({ status: 200, statusText: 'OK', body: [wave] });
    if (request.path.includes('/api/waves/')) return Promise.resolve({ status: 200, statusText: 'OK', body: { wave, cards: [], overlays: [] } });
    return Promise.resolve({ status: 200, statusText: 'OK', body: [] });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  const rail = await screen.findByRole('navigation', { name: 'Workspace' });
  await userEvent.click(await within(rail).findByText('Encoded wave'));
  // TanStack normalises %20 back to a space in memory history while retaining
  // the escapes that delimit the segment; useRouteParam still restores all of it.
  expect(router.state.location.pathname).toBe('/wave/a%2Fb %25');
  expect(within(rail).getByRole('button', { name: /Wave Encoded wave/ }).getAttribute('aria-current')).toBe('page');
  await screen.findByRole('button', { name: 'Rename wave' });
  expect(requests.some((request) => request.path.includes('a%2Fb%20%25'))).toBe(true);
});
