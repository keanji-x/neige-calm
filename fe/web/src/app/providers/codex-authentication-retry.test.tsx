// @vitest-environment jsdom
import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { agentProvidersQueryOptions } from './agent-providers.ts';
import { useCodexAuthenticationRetry } from './codex-authentication-retry.ts';
afterEach(cleanup);
it('double clicks send one CAS intent and re-read status after its receipt without declaring login success', async () => {
  const sent: ApiRequest[] = [];
  let release!: (response: ApiTransportResponse) => void;
  const deferred = new Promise<ApiTransportResponse>((resolve) => { release = resolve; });
  const transport: ApiTransportPort = { send: (request) => {
    sent.push(request);
    return request.method === 'POST' ? deferred : Promise.resolve({ status: 200, statusText: 'OK', body: [] });
  } };
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const hook = renderHook(() => {
    useQuery(agentProvidersQueryOptions(transport, unauthorized));
    return useCodexAuthenticationRetry(transport, unauthorized);
  }, { wrapper: ({ children }) => <QueryClientProvider client={client}>{children}</QueryClientProvider> });
  await waitFor(() => expect(sent.some((request) => request.method === 'GET')).toBe(true));
  act(() => { hook.result.current.retry('7'); hook.result.current.retry('7'); });
  await waitFor(() => expect(sent.filter((request) => request.method === 'POST')).toHaveLength(1));
  expect(sent.find((request) => request.method === 'POST')?.body).toEqual({ expected_revision: '7' });
  release({ status: 200, statusText: 'OK', body: { status: 'retry_requested', requested_revision: '8', recovery_notices: [] } });
  await waitFor(() => expect(hook.result.current.pending).toBe(false));
  await waitFor(() => expect(sent.filter((request) => request.method === 'GET').length).toBeGreaterThan(1));
  expect(hook.result.current.error).toBeNull();
});
