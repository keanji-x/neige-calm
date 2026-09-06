// @vitest-environment jsdom
//
// One claim: a settings PUT's response is **not** the cache.
//
// It used to be written straight through, which is only sound while writes
// cannot overlap. Settings › Network commits per field, so two writes to one
// key overlap routinely, and the older response can land last — after which the
// cache held a value the server had already replaced, and the field visibly
// reverted under a green tick.
import { onlineManager, QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { HTTP_PROXY_KEY, TASK_BUDGET_DEFAULT_KEY } from '../../../../core/domain/settings.ts';
import { GeneralPane } from '../../features/settings/public.tsx';
import { settingsQueryOptions, useSettingsMutation } from './queries.ts';

afterEach(() => { cleanup(); onlineManager.setOnline(true); vi.useRealTimers(); });
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

function SettingsProbe({ transport }: { transport: ApiTransportPort }) {
  const onSave = useSettingsMutation(transport, unauthorized);
  const query = useQuery(settingsQueryOptions(transport, unauthorized));
  return <GeneralPane settings={query.data?.settings} loadError={null} onSave={async (patch) => { await onSave(patch); }}
    onRetryLoad={() => { void query.refetch(); }} savedNoticeMs={60_000} />;
}

it('rejects an offline Settings write without replaying it over a newer client value', async () => {
  let serverValue = '1';
  const writes: string[] = [];
  const transport: ApiTransportPort = { send: (request) => {
    if (request.method === 'PUT') {
      const body = request.body as { settings: Record<string, string> };
      serverValue = body.settings[TASK_BUDGET_DEFAULT_KEY];
      writes.push(serverValue);
    }
    return Promise.resolve({ status: 200, statusText: 'OK', body: { settings: { [TASK_BUDGET_DEFAULT_KEY]: serverValue } } });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><SettingsProbe transport={transport} /></QueryClientProvider>);
  const field = await screen.findByLabelText('Task concurrency');
  act(() => onlineManager.setOnline(false));
  await userEvent.clear(field); await userEvent.type(field, '2'); await userEvent.tab();
  await screen.findByText(/offline.*Reconnect/i);
  serverValue = '3'; // A second independent client committed this while this tab was offline.
  await act(async () => { onlineManager.setOnline(true); await client.resumePausedMutations(); });
  expect(serverValue).toBe('3');
  expect(writes).toEqual([]);
  expect(screen.getByLabelText<HTMLInputElement>('Task concurrency').value).toBe('2');
});

it('refreshes an open settings pane after another client writes and removes its obsolete Saved notice', async () => {
  vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
  let serverValue = '1';
  const transport: ApiTransportPort = { send: (request) => {
    if (request.method === 'PUT') serverValue = (request.body as { settings: Record<string, string> }).settings[TASK_BUDGET_DEFAULT_KEY];
    return Promise.resolve({ status: 200, statusText: 'OK', body: { settings: { [TASK_BUDGET_DEFAULT_KEY]: serverValue } } });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><SettingsProbe transport={transport} /></QueryClientProvider>);
  const field = await screen.findByLabelText('Task concurrency');
  await userEvent.clear(field); await userEvent.type(field, '3'); await userEvent.tab();
  await waitFor(() => expect(within(field.closest('li')!).getByRole('status').textContent).toBe('Saved.'));
  serverValue = '2';
  await act(async () => { await vi.advanceTimersByTimeAsync(16_000); });
  vi.useRealTimers();
  await waitFor(() => expect(screen.getByLabelText<HTMLInputElement>('Task concurrency').value).toBe('2'));
  expect(field.closest('li')?.textContent).not.toContain('Saved.');
});

it('does not let an older PUT response overwrite a newer one', async () => {
  const puts: Array<() => void> = [];
  let serverValue = 'seed';
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      if (request.method === 'GET') {
        return Promise.resolve({
          status: 200, statusText: 'OK', body: { settings: { [HTTP_PROXY_KEY]: serverValue } },
        });
      }
      // The server applies each write as it arrives; the *responses* are what
      // this test delays and reorders.
      const body = request.body as { settings: Record<string, string> };
      serverValue = body.settings[HTTP_PROXY_KEY] ?? '';
      const echoed = { status: 200, statusText: 'OK', body: { settings: { [HTTP_PROXY_KEY]: serverValue } } };
      return new Promise<ApiTransportResponse>((resolve) => { puts.push(() => resolve(echoed)); });
    },
  };

  let save: ((patch: Record<string, string | null>) => Promise<unknown>) | null = null;
  function Probe() {
    save = useSettingsMutation(transport, unauthorized);
    const settings = useQuery(settingsQueryOptions(transport, unauthorized));
    return <span data-testid="cached">{settings.data?.settings[HTTP_PROXY_KEY] ?? ''}</span>;
  }
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><Probe /></QueryClientProvider>);
  await act(async () => { await Promise.resolve(); });

  await act(async () => { void save?.({ [HTTP_PROXY_KEY]: 'ab' }); await Promise.resolve(); });
  await act(async () => { void save?.({ [HTTP_PROXY_KEY]: 'abc' }); await Promise.resolve(); });
  // The newer write answers first, the older one last.
  await act(async () => { puts[1]?.(); await Promise.resolve(); });
  await act(async () => { puts[0]?.(); await Promise.resolve(); });
  // Let the invalidation's refetch settle.
  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); });

  expect(serverValue).toBe('abc');
  expect(screen.getByTestId('cached').textContent).toBe('abc');
});
