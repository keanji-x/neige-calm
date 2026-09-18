import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { usePluginConfigMutations } from './queries.ts';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
it('a restart interrupted during readback cannot confirm the old running state', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true); const access = new RecoveryAccess(); access.change('connected');
  const requests: ApiRequest[] = []; let finish!: (response: ApiTransportResponse) => void;
  const detail = { id: 'plug', version: '1', enabled: true, state: 'running', user_config: {}, effective_config: {} };
  const response = { status: 200, statusText: 'OK', body: detail };
  const transport = createRecoveryTransports({ send: request => {
    requests.push(request);
    return request.method === 'POST' ? Promise.resolve(response) : new Promise(resolve => { finish = resolve; });
  } }, access).business;
  const client = new QueryClient();
  const hook = renderHook(() => usePluginConfigMutations(transport, createUnauthorizedChannel({ enqueue: task => task() })), {
    wrapper: ({ children }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>,
  });
  const result = hook.result.current.applyRestart('plug', {}, { reset: false });
  await waitFor(() => expect(requests).toHaveLength(2));
  act(() => access.invalidate('paused')); finish(response);
  const outcome = await result;
  expect(outcome).toMatchObject({ saved: true, restart: { state: 'unknown', failure: { code: 'transport_failure' } } });
  expect(requests.filter(request => request.method === 'POST')).toHaveLength(1);
});
