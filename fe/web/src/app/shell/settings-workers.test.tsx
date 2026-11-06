// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import { queryKeys } from '../providers/queries.ts';
import { CurrentUserScope } from '../auth/current-user.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

function setup({ path = '/settings', compact = false, userId = 'local-owner', reply }: {
  path?: string; compact?: boolean; userId?: string | null; reply?: (request: ApiRequest) => Promise<ApiTransportResponse>;
} = {}) {
  vi.stubGlobal('matchMedia', vi.fn(() => ({ matches: compact, addEventListener: vi.fn(), removeEventListener: vi.fn() })));
  const sent: ApiRequest[] = [];
  const transport: ApiTransportPort = { send(request) {
    sent.push(request);
    if (reply && (request.path === '/api/settings' || request.path.startsWith('/api/models'))) return reply(request);
    return Promise.resolve(ok(request.path === '/api/settings' ? { settings: {} } : []));
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport,
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), client,
    cards: bootTestCardRuntime(), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    {userId === null ? <RouterProvider router={router} /> : <CurrentUserScope value={{ userId, displayName: 'Person', role: 'owner', sessionId: 'session' }}>
      <RouterProvider router={router} />
    </CurrentUserScope>}
  </ThemeProvider></QueryClientProvider>);
  return { sent, client, router };
}

it.each([false, true])('opens Workers through the existing Settings navigation (compact=%s)', async compact => {
  const { router } = setup({ compact });
  await screen.findByRole('navigation', { name: compact ? 'Settings categories' : 'Settings sections' });
  await userEvent.click(screen.getByRole('button', { name: 'Workers' }));
  await waitFor(() => expect(router.state.location.pathname).toBe('/settings/workers'));
  expect(await screen.findByRole('textbox', { name: 'Codex model' })).toBeTruthy();
  if (compact) {
    await userEvent.click(screen.getByRole('button', { name: 'Back to Settings' }));
    await waitFor(() => expect(router.state.location.pathname).toBe('/settings'));
  }
});

it.each([false, true])('writes and clears one provider pair and rereads the same Settings bag (compact=%s)', async compact => {
  let stored: Record<string, string> = { http_proxy: 'http://proxy', planner_max_turn_duration_minutes: '45' };
  const { sent, router } = setup({ compact, path: '/settings/workers', reply: request => {
    if (request.method === 'PUT') {
      const patch = (request.body as { settings: Record<string, string | null> }).settings;
      for (const [key, value] of Object.entries(patch)) {
        if (value === null) delete stored[key]; else stored[key] = value;
      }
      stored = { ...stored };
    }
    return Promise.resolve(ok({ settings: stored }));
  } });
  await screen.findByRole('textbox', { name: 'Codex model' });
  await userEvent.type(screen.getByLabelText('Codex model'), 'custom-codex');
  await userEvent.type(screen.getByLabelText('Codex effort'), 'custom-effort');
  await userEvent.type(screen.getByLabelText('Claude model'), 'custom-claude');
  await userEvent.tab();
  expect(sent.filter(request => request.method === 'PUT')).toHaveLength(0);
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  await userEvent.click(screen.getByRole('button', { name: 'Save Claude defaults' }));
  await waitFor(() => expect(sent.filter(request => request.method === 'PUT')).toHaveLength(2));
  expect(sent.filter(request => request.method === 'PUT').map(request => request.body)).toEqual([
    { settings: { worker_model_defaults_codex: '{"model":"custom-codex","effort":"custom-effort"}' } },
    { settings: { worker_model_defaults_claude: '{"model":"custom-claude","effort":null}' } },
  ]);
  await act(() => router.navigate({ to: '/settings/about' }));
  await act(() => router.navigate({ to: '/settings/workers' }));
  expect((await screen.findByLabelText<HTMLInputElement>('Codex model')).value).toBe('custom-codex');
  expect((screen.getByLabelText<HTMLInputElement>('Codex effort')).value).toBe('custom-effort');
  expect((screen.getByLabelText<HTMLInputElement>('Claude model')).value).toBe('custom-claude');
  await userEvent.click(screen.getByRole('button', { name: 'Clear Codex defaults' }));
  await waitFor(() => expect((screen.getByLabelText<HTMLInputElement>('Codex model')).value).toBe(''));
  expect(sent.filter(request => request.method === 'PUT').at(-1)?.body).toEqual({ settings: { worker_model_defaults_codex: null } });
  expect(stored).toEqual({ http_proxy: 'http://proxy', planner_max_turn_duration_minutes: '45',
    worker_model_defaults_claude: '{"model":"custom-claude","effort":null}' });
  expect(sent.filter(request => request.path === '/api/settings' && request.method === 'GET').length).toBeGreaterThan(1);
});

it.each(['different-user', null])('offers no editor or query to identity %s even with the owner role label', async userId => {
  const { sent } = setup({ path: '/settings/workers', userId });
  await screen.findByText('Only the deployment owner can edit Worker defaults.');
  expect(screen.queryAllByRole('textbox')).toHaveLength(0);
  expect(screen.queryByRole('button', { name: 'Save Codex defaults' })).toBeNull();
  expect(sent.filter(request => request.path === '/api/settings' || request.path.startsWith('/api/models'))).toHaveLength(0);
});

it('waits for Settings, and a later forbidden read revokes editing even with cached data', async () => {
  let release!: (value: ApiTransportResponse) => void;
  let forbidden = false;
  const { client, sent } = setup({ path: '/settings/workers', reply: request => {
    if (request.path !== '/api/settings') return Promise.resolve(ok([]));
    return forbidden ? Promise.resolve({ status: 403, statusText: 'Forbidden', body: { code: 'forbidden', error: 'Owner only' } })
      : new Promise(resolve => { release = resolve; });
  } });
  await screen.findByText('Loading settings…');
  expect(screen.queryAllByRole('textbox')).toHaveLength(0);
  act(() => release(ok({ settings: { worker_model_defaults_codex: '{"model":"stored","effort":null}' } })));
  await screen.findByLabelText('Codex model');
  forbidden = true;
  await act(() => client.refetchQueries({ queryKey: queryKeys.settings() }));
  await screen.findByText(/Owner only/);
  expect((screen.getByLabelText<HTMLInputElement>('Codex model')).disabled).toBe(true);
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  expect(sent.filter(request => request.method === 'PUT')).toHaveLength(0);
});

it.each([
  ['refused', 400, 'It was not saved.'], ['forbidden', 403, 'It was not saved.'],
  ['server failure', 500, 'The save is unconfirmed.'], ['decode failure', 200, 'The save is unconfirmed.'],
  ['lost response', 0, 'The save is unconfirmed.'],
] as const)('retains the draft on %s and requires an explicit retry', async (_name, status, message) => {
  const { sent } = setup({ path: '/settings/workers', reply: request => {
    if (request.method !== 'PUT') return Promise.resolve(ok({ settings: {} }));
    if (status === 0) return Promise.reject(new Error('private transport details'));
    return Promise.resolve({ status, statusText: 'Error', body: { code: 'bad_request', field: 'settings.worker_model_defaults_codex', error: 'Pair rejected' } });
  } });
  await userEvent.type(await screen.findByLabelText('Codex model'), 'custom');
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  await screen.findByText(text => text.includes(message));
  await userEvent.click(screen.getByLabelText('Codex model'));
  await userEvent.keyboard('{Enter}{Tab}');
  expect((screen.getByLabelText<HTMLInputElement>('Codex model')).value).toBe('custom');
  expect(sent.filter(request => request.method === 'PUT')).toHaveLength(1);
  expect(screen.queryByText('Saved.')).toBeNull();
  expect(screen.queryByText(/private transport details/)).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  await waitFor(() => expect(sent.filter(request => request.method === 'PUT')).toHaveLength(2));
});

it('never installs a late PUT bag into the shared cache or confirms a newer draft', async () => {
  let release!: (response: ApiTransportResponse) => void;
  let raw = '{"model":"original","effort":null}';
  const { client } = setup({ path: '/settings/workers', reply: request => request.method === 'PUT'
    ? new Promise(resolve => { release = resolve; })
    : Promise.resolve(ok({ settings: { worker_model_defaults_codex: raw } })) });
  const field = await screen.findByLabelText('Codex model');
  await userEvent.clear(field);
  await userEvent.type(field, 'submitted');
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  await userEvent.type(field, '-new');
  raw = '{"model":"other-client","effort":null}';
  await act(() => client.refetchQueries({ queryKey: queryKeys.settings() }));
  act(() => release(ok({ settings: { worker_model_defaults_codex: '{"model":"submitted","effort":null}' } })));
  await waitFor(() => expect((screen.getByRole<HTMLButtonElement>('button', { name: 'Save Codex defaults' })).disabled).toBe(false));
  expect((field as HTMLInputElement).value).toBe('submitted-new');
  expect(screen.queryByText('Saved.')).toBeNull();
  expect(client.getQueryData(queryKeys.settings())).toEqual({ settings: { worker_model_defaults_codex: raw } });
});

it('a successful unchanged GET supersedes a stale acknowledgement under a frozen clock', async () => {
  const clock = vi.spyOn(Date, 'now').mockReturnValue(1000);
  try {
    const { client } = setup({ path: '/settings/workers', reply: request => Promise.resolve(ok({ settings: {
      worker_model_defaults_codex: request.method === 'PUT' ? '{"model":"submitted","effort":null}' : '{"model":"original","effort":null}',
    } })) });
    const field = await screen.findByLabelText('Codex model');
    await userEvent.clear(field);
    await userEvent.type(field, 'submitted');
    await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
    await waitFor(() => expect(client.getQueryState(queryKeys.settings())!.dataUpdateCount).toBeGreaterThan(1));
    await waitFor(() => expect(screen.queryByText('Saved.')).toBeNull());
    expect((field as HTMLInputElement).value).toBe('submitted');
  } finally { clock.mockRestore(); }
});
