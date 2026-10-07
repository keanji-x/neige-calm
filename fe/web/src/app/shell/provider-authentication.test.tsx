// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { ProviderAvailability } from '../../../../core/domain/agent-providers.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { queryKeys } from '../providers/queries.ts';
import { ProviderAuthenticationNotice } from './provider-authentication.tsx';
afterEach(cleanup);
const REQUIRED = 'Codex sign-in needs renewal. Sign in again for this server, then retry.';
const REPORTED = 'Codex reported a sign-in renewal error. This does not confirm that your current sign-in has failed.';
function entry(kind: 'sign_in_required' | 'refresh_error_reported' | null): ProviderAvailability {
  return kind === 'sign_in_required'
    ? { provider: 'codex', status: 'unavailable', reason: REQUIRED, checked_at_ms: 1, authentication_notice: { kind, revision: '1', text: REQUIRED } }
    : { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: kind === null ? null : { kind, revision: '1', text: REPORTED } };
}
function mount(initial: ProviderAvailability) {
  let answer = initial;
  let failing = false;
  const requests: string[] = [];
  const transport: ApiTransportPort = { send: (request) => {
    requests.push(`${request.method} ${request.path}`);
    if (failing) return Promise.reject(new Error('network unavailable'));
    return Promise.resolve({ status: 200, statusText: 'OK', body: [answer] });
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const open = vi.fn();
  render(<QueryClientProvider client={client}><ProviderAuthenticationNotice transport={transport}
    unauthorized={createUnauthorizedChannel({ enqueue: (task) => task() })} onOpenPlanners={open} /></QueryClientProvider>);
  return { client, open, requests, update: (next: ProviderAvailability) => { answer = next; }, fail: () => { failing = true; } };
}
it('surfaces a report while ready and performs no login or reset', async () => {
  const app = mount(entry('refresh_error_reported'));
  await screen.findByText(REPORTED);
  expect(screen.getByText('Messages can still run. Check the server’s sign-in if they stop.')).toBeTruthy();
  await userEvent.click(screen.getByRole('button', { name: 'Settings' }));
  expect(app.open).toHaveBeenCalledOnce();
  expect(app.requests).toEqual(['GET /api/agent-providers']);
});
it('keeps queued reassurance and clears on a later server answer', async () => {
  const app = mount(entry('sign_in_required'));
  await screen.findByText(REQUIRED);
  expect(screen.getByText('Queued messages are kept. After signing in for this server, open Settings to allow them to retry.')).toBeTruthy();
  app.update(entry(null));
  await app.client.invalidateQueries({ queryKey: queryKeys.agentProviders() });
  await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
});
it('retains confirmed warning when a read fails', async () => {
  const app = mount(entry('sign_in_required'));
  await screen.findByText(REQUIRED);
  app.fail();
  await app.client.invalidateQueries({ queryKey: queryKeys.agentProviders() });
  expect(screen.getByText(REQUIRED)).toBeTruthy();
});
it('does not invent authentication evidence from ordinary unavailability', async () => {
  const app = mount({ provider: 'codex', status: 'unavailable', reason: 'daemon starting', checked_at_ms: 1, authentication_notice: null });
  await waitFor(() => expect(app.client.getQueryData(queryKeys.agentProviders())).toBeDefined());
  expect(screen.queryByRole('alert')).toBeNull();
});
