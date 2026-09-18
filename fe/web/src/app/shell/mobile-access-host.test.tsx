import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { MobileAccessHost } from './mobile-access-host.tsx';

afterEach(cleanup);

function mount(reply: (request: ApiRequest) => Promise<ApiTransportResponse>, customCleanup = false) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const send = vi.fn((request: ApiRequest) => !customCleanup && request.method === 'GET' && request.path === '/api/mobile/enrollments'
    ? Promise.resolve(ok({ pendingCleanup: 0, detail: 'No pending cleanup' })) : reply(request));
  const ui = render(<QueryClientProvider client={client}><MobileAccessHost
    transport={{ send }} unauthorized={createUnauthorizedChannel({ enqueue: (task) => task() })} onBack={() => undefined}
  /></QueryClientProvider>);
  return { ...ui, send, client };
}
function status() {
  return {
    provider: 'private-tailnet', available: true, publicUrl: 'https://fixture.example.ts.net', pending: [], devices: [],
    tailnet: { desiredEnabled: true, phase: 'online', nodeState: 'online', processRunning: true, childPid: 123,
      httpsReady: true, upstreamReady: true, origin: 'https://fixture.example.ts.net', dnsName: 'fixture.example.ts.net',
      nodeId: 'fixture', addresses: ['100.64.0.1'], detail: 'Private HTTPS ready' },
  };
}
function enrollment() {
  return { enrollmentId: 'new-scan', qrPayload: 'neige-enroll:v2:fixture', qrImage: 'data:image/svg+xml;base64,PHN2Zy8+',
    authKeyExpiresAt: Date.now() + 300_000, pairExpiresAt: Date.now() + 180_000 };
}
function ok(body: unknown): ApiTransportResponse { return { status: 200, statusText: 'OK', body }; }

it('uses the v2 production API once and cancels before showing cleanup status', async () => {
  const view = mount((request) => {
    if (request.path === '/api/mobile/enrollments') return Promise.resolve(ok(enrollment()));
    if (request.method === 'DELETE') return Promise.resolve(ok({ pendingCleanup: 1, detail: 'Cloud cleanup pending; real expiry retained' }));
    return Promise.resolve(ok(status()));
  });
  fireEvent.click(await screen.findByRole('button', { name: 'Add phone' }));
  expect(await screen.findByAltText('Scan once to join and pair this Neige workspace')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: 'Cancel invitation' }));
  await screen.findByText('Cloud cleanup pending; real expiry retained');
  expect(screen.queryByAltText('Scan once to join and pair this Neige workspace')).toBeNull();
  expect(view.send.mock.calls.filter(([r]) => r.method === 'POST').map(([r]) => r.path)).toEqual(['/api/mobile/enrollments']);
  expect(view.send.mock.calls.filter(([r]) => r.method === 'DELETE').map(([r]) => r.path)).toEqual(['/api/mobile/enrollments/new-scan']);
});

it('retires a late QR after the Settings host unmounts', async () => {
  let resolveIssued: (response: ApiTransportResponse) => void = () => { throw new Error('Uninitialized fixture'); };
  const issued = new Promise<ApiTransportResponse>((resolve) => { resolveIssued = resolve; });
  const view = mount((request) => {
    if (request.path === '/api/mobile/enrollments') return issued;
    if (request.method === 'DELETE') return Promise.resolve(ok({ pendingCleanup: 0, detail: 'Cleaned' }));
    return Promise.resolve(ok(status()));
  });
  fireEvent.click(await screen.findByRole('button', { name: 'Add phone' }));
  await waitFor(() => expect(view.send.mock.calls.some(([r]) => r.method === 'POST' && r.path === '/api/mobile/enrollments')).toBe(true));
  view.unmount();
  resolveIssued(ok(enrollment()));
  await waitFor(() => expect(view.send.mock.calls.filter(([r]) => r.method === 'DELETE').map(([r]) => r.path)).toEqual(['/api/mobile/enrollments/new-scan']));
  expect(screen.queryByAltText('Scan once to join and pair this Neige workspace')).toBeNull();
});

it('does not retry an uncertain create or display an invented expiry', async () => {
  const view = mount((request) => Promise.resolve(request.path === '/api/mobile/enrollments'
    ? { status: 400, statusText: 'Bad request', body: { code: 'bad_request', error: 'Key result unknown; administrator reconciliation required' } }
    : ok(status())));
  fireEvent.click(await screen.findByRole('button', { name: 'Add phone' }));
  await screen.findByRole('alert');
  expect(view.send.mock.calls.filter(([r]) => r.method === 'POST')).toHaveLength(1);
  expect(screen.queryByAltText('Scan once to join and pair this Neige workspace')).toBeNull();
});

it('reads pending cloud cleanup and its actual expiry while remote access is disabled', async () => {
  const disabled = status();
  disabled.tailnet.desiredEnabled = false;
  disabled.tailnet.processRunning = false;
  const message = 'Cloud key cleanup pending; returned expiry 2026-09-18T18:05:00Z';
  const view = mount((request) => Promise.resolve(ok(request.path === '/api/mobile/enrollments'
    ? { pendingCleanup: 1, detail: message } : disabled)), true);
  await screen.findByText(message);
  expect(view.send.mock.calls.some(([r]) => r.path === '/api/mobile/enrollments' && r.method === 'GET')).toBe(true);
  expect(view.send.mock.calls.every(([r]) => r.method === 'GET')).toBe(true);
});
