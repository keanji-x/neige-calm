// @vitest-environment jsdom
//
// One claim: a settings PUT's response is **not** the cache.
//
// It used to be written straight through, which is only sound while writes
// cannot overlap. Settings › Network commits per field, so two writes to one
// key overlap routinely, and the older response can land last — after which the
// cache held a value the server had already replaced, and the field visibly
// reverted under a green tick.
import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { HTTP_PROXY_KEY } from '../../../../core/domain/settings.ts';
import { settingsQueryOptions, useSettingsMutation } from './queries.ts';

afterEach(cleanup);
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

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
