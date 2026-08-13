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
