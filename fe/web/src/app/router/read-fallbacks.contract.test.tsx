// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const coves = [
  { id: 'c1', name: 'One', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 },
  { id: 'c2', name: 'Two', color: '#654321', sort: 2, kind: 'user', created_at: 1, updated_at: 1 },
];
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const wave = { id: 'w1', cove_id: 'c1', title: 'Reliable', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1 };
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const fail = (message: string): ApiTransportResponse => ({ status: 500, statusText: 'Server Error', body: { error: message } });

/* #1253 — none of the cases in this file are about the Today launchpad, and
   404 is that endpoint's ordinary "no launchpad yet" answer. Answering it here
   rather than letting each case's catch-all `ok([])` reach it keeps a decode
   failure out of every Today render below; a case that wants the resolve to
   fail says so by handling the path itself. */
const TODAY_LAUNCHPAD_PATH = '/api/today/launchpad';
const notFound = (): ApiTransportResponse => ({ status: 404, statusText: 'Not Found', body: { error: 'not found' } });

function renderRoute(path: string, reply: (request: ApiRequest) => ApiTransportResponse | Promise<ApiTransportResponse>) {
  const transport: ApiTransportPort = {
    send: (request) => Promise.resolve(request.path === TODAY_LAUNCHPAD_PATH ? notFound() : reply(request)),
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  const view = render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { ...view, client };
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
    const main = await screen.findByRole('main');
    expect((await within(main).findAllByRole('alert')).some((node) => node.textContent?.includes('Wave activity is unavailable: overlays down'))).toBe(true);
  });

  it('keeps Today content when one cove wave read fails', async () => {
    renderRoute('/', (request) => {
      if (request.path === '/api/coves') return ok(coves);
      if (request.path === '/api/coves/c1/waves') return ok([wave]);
      if (request.path === '/api/coves/c2/waves') return fail('cove two down');
      return ok([]);
    });
    expect((await screen.findAllByText('Reliable')).length).toBeGreaterThan(1);
    expect(within(screen.getByRole('main')).getAllByRole('alert').some((node) => node.textContent?.includes('cove two down'))).toBe(true);
    expect(within(screen.getByRole('main')).getByRole('heading', { level: 1 })).toBeTruthy();
  });

  it('keeps Cove content when a refetch fails after usable wave data', async () => {
    let waveReads = 0;
    const view = renderRoute('/cove/c1', (request) => {
      if (request.path === '/api/coves') return ok(coves.slice(0, 1));
      if (request.path === '/api/coves/c1/waves') return ++waveReads === 1 ? ok([wave]) : fail('waves stale');
      return ok([]);
    });
    expect(await screen.findByRole('button', { name: 'New wave' })).toBeTruthy();
    await view.client.invalidateQueries({ queryKey: ['waves', 'c1'] });
    expect(await within(screen.getByRole('main')).findByText('waves stale')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'New wave' })).toBeTruthy();
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

  it('uses a successful neutral detail read instead of stale workspace activity', async () => {
    renderRoute('/wave/w1', (request) => {
      if (request.path === '/api/coves') return ok(coves.slice(0, 1));
      if (request.path === '/api/coves/c1/waves') return ok([wave]);
      if (request.path.startsWith('/api/overlays?')) return ok([{
        id: 'workspace-needs-input', plugin_id: 'cards', entity_kind: 'wave', entity_id: 'w1',
        kind: 'any_card_needs_input', payload: { value: true }, updated_at: 1,
      }]);
      if (request.path === '/api/waves/w1') return ok({ wave, cards: [], overlays: [] });
      return ok([]);
    });
    await screen.findByRole('button', { name: 'Rename wave' });
    expect(screen.queryByText('Needs input')).toBeNull();
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
  const todayContent = within(screen.getByRole('main')).getByRole('heading', { level: 1 });
  expect(alert.compareDocumentPosition(todayContent) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  await userEvent.click(within(alert).getByRole('button', { name: 'Dismiss' }));
  expect(screen.queryByRole('alert')).toBeNull();
});
