// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';

const coves = [
  { id: 'c1', name: 'One', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
  { id: 'c2', name: 'Two', color: '#654321', sort: 2, kind: 'user', created_at: 1, updated_at: 1 },
];
const wave = { id: 'w1', cove_id: 'c1', title: 'Reliable', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1 };
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const fail = (message: string): ApiTransportResponse => ({ status: 500, statusText: 'Server Error', body: { error: message } });

function renderRoute(path: string, reply: (request: ApiRequest) => ApiTransportResponse | Promise<ApiTransportResponse>) {
  const transport: ApiTransportPort = { send: (request) => Promise.resolve(reply(request)) };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  return render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe('degraded workspace reads stay usable', () => {
  it.each([['Today', '/'], ['Cove', '/cove/c1']])('%s warns when activity is unavailable', async (_name, path) => {
    renderRoute(path, (request) => {
      if (request.path === '/api/coves') return ok(coves.slice(0, 1));
      if (request.path === '/api/coves/c1/waves') return ok([wave]);
      if (request.path.startsWith('/api/overlays?')) return fail('overlays down');
      return ok([]);
    });
    expect((await screen.findAllByRole('alert')).some((node) => node.textContent?.includes('Wave activity is unavailable: overlays down'))).toBe(true);
  });

  it('keeps Today content when one cove wave read fails', async () => {
    renderRoute('/', (request) => {
      if (request.path === '/api/coves') return ok(coves);
      if (request.path === '/api/coves/c1/waves') return ok([wave]);
      if (request.path === '/api/coves/c2/waves') return fail('cove two down');
      return ok([]);
    });
    expect((await screen.findAllByText('Reliable')).length).toBeGreaterThan(1);
    expect(screen.getAllByRole('alert').some((node) => node.textContent?.includes('cove two down'))).toBe(true);
    expect(screen.getByText('Terminal is not wired up yet.')).toBeTruthy();
  });

  it('prefers wave-detail overlays to the neutral workspace fallback', async () => {
    let resolveDetail: (response: ApiTransportResponse) => void = () => undefined;
    const detail = new Promise<ApiTransportResponse>((resolve) => { resolveDetail = resolve; });
    renderRoute('/wave/w1', (request) => {
      if (request.path === '/api/coves') return ok(coves.slice(0, 1));
      if (request.path === '/api/coves/c1/waves') return ok([wave]);
      if (request.path.startsWith('/api/overlays?')) return fail('overlays down');
      if (request.path === '/api/waves/w1') return detail;
      return ok([]);
    });
    await within(await screen.findByRole('navigation', { name: 'Workspace' })).findByText('Reliable');
    resolveDetail(ok({ wave, cards: [], overlays: [{
      id: 'o1', plugin_id: 'cards', entity_kind: 'wave', entity_id: 'w1',
      kind: 'any_card_needs_input', payload: { value: true }, updated_at: 1,
    }] }));
    expect(await screen.findByText('Needs input')).toBeTruthy();
  });
});

it('puts a dismissible delete failure before Today content', async () => {
  renderRoute('/', (request) => {
    if (request.path === '/api/coves') return ok(coves.slice(0, 1));
    if (request.path === '/api/coves/c1/waves') return ok([wave]);
    if (request.path.startsWith('/api/overlays?')) return ok([]);
    if (request.method === 'DELETE') return fail('wave changed elsewhere');
    return ok([]);
  });
  const rail = await screen.findByRole('complementary');
  await userEvent.click(await within(rail).findByRole('button', { name: 'Delete Reliable' }));
  await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));
  const alert = await screen.findByRole('alert');
  const todayContent = screen.getByText('Terminal is not wired up yet.');
  expect(alert.compareDocumentPosition(todayContent) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  await userEvent.click(within(alert).getByRole('button', { name: 'Dismiss' }));
  expect(screen.queryByRole('alert')).toBeNull();
});
