// @vitest-environment jsdom
import { QueryClient, QueryClientProvider, useQuery, onlineManager } from '@tanstack/react-query';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { agentProvidersQueryOptions } from './agent-providers.ts';
import { useCodexAuthenticationRetry } from './codex-authentication-retry.ts';
afterEach(() => { cleanup(); onlineManager.setOnline(true); });
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

for (const status of [400, 403, 409]) {
  it(`shows HTTP ${status} as a definite refusal rather than an unconfirmed retry`, async () => {
    const transport: ApiTransportPort = { send: () => Promise.resolve({ status, statusText: 'Refused', body: { error: 'refused' } }) };
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const hook = renderHook(() => useCodexAuthenticationRetry(transport, createUnauthorizedChannel({ enqueue: (task) => task() })),
      { wrapper: ({ children }) => <QueryClientProvider client={client}>{children}</QueryClientProvider> });
    act(() => { hook.result.current.retry('revision'); });
    await waitFor(() => expect(hook.result.current.error).toContain('not allowed'));
  });
}
it('shows an offline retry as not sent and does not send a POST', async () => {
  const sent: ApiRequest[] = [];
  const transport: ApiTransportPort = { send: (request) => { sent.push(request); return Promise.resolve({ status: 200, statusText: 'OK', body: {} }); } };
  const client = new QueryClient();
  onlineManager.setOnline(false);
  const hook = renderHook(() => useCodexAuthenticationRetry(transport, createUnauthorizedChannel({ enqueue: (task) => task() })),
    { wrapper: ({ children }) => <QueryClientProvider client={client}>{children}</QueryClientProvider> });
  act(() => { hook.result.current.retry('revision'); });
  await waitFor(() => expect(hook.result.current.error).toContain('Not sent'));
  expect(sent).toEqual([]);
});
