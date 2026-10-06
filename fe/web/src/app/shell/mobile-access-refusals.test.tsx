import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiRequest, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { IDB_DB_NAME } from '../../../../core/keys/storage.ts';
import { MOBILE_WRITE_TEXT } from '../../../../core/domain/mobile-access.ts';
import { SessionGate } from '../auth/session-gate.tsx';
import { MobileAccessHost } from './mobile-access-host.tsx';

afterEach(cleanup);

const identity = { userId: 'owner', displayName: 'Owner', role: 'admin', sessionId: 'owner-session' };
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const answered = (status: number, code: string, error: string): ApiTransportResponse =>
  ({ status, statusText: 'Refused', body: { code, error } });
/* The list a second tab still shows: the phone and the pending request are already gone on the server. */
const stale = {
  provider: 'funnel', tailnet: null, available: true, publicUrl: 'https://fixture.example.ts.net',
  pending: [{ id: 'handled', deviceName: 'Old phone', verificationCode: '123456' }],
  devices: [{ id: 'gone', deviceName: 'Phone' }],
};

/** The production session gate around the production host; `write` answers every mobile write. */
function mount(write: (request: ApiRequest) => Promise<ApiTransportResponse>) {
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const signedOut = vi.fn();
  unauthorized.subscribe(signedOut);
  const send = vi.fn((request: ApiRequest) => {
    if (request.path === '/api/auth/whoami') return Promise.resolve(ok(identity));
    if (request.method === 'GET') return Promise.resolve(ok(stale));
    return write(request);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>
    <SessionGate transport={{ send }} unauthorized={unauthorized} client={client}
      runtime={{ deleteDatabase: vi.fn(), idbDatabaseName: IDB_DB_NAME }} cursorStore={{ clear: vi.fn() }}
      renderLogin={() => <b>login</b>} renderError={() => <b>session error</b>}>
      <MobileAccessHost transport={{ send }} unauthorized={unauthorized} onBack={() => undefined} />
    </SessionGate>
  </QueryClientProvider>);
  const reads = () => send.mock.calls.filter(([r]) => r.method === 'GET' && r.path === '/api/mobile/access').length;
  return { send, signedOut, reads };
}

function sessionKept(view: ReturnType<typeof mount>) {
  expect(view.signedOut).not.toHaveBeenCalled();
  expect(screen.queryByText('login')).toBeNull();
  expect(screen.getByRole('button', { name: 'Revoke' })).toBeTruthy();
}

it('a double Revoke is done: the second answer, the device already gone, keeps the session and shows no failure', async () => {
  const revokes: string[] = [];
  const view = mount((request) => {
    revokes.push(request.path);
    return Promise.resolve(revokes.length === 1
      ? { status: 204, statusText: 'No Content', body: null }
      : answered(404, 'not_found', 'No paired device with this id'));
  });
  for (const press of [1, 2]) {
    const reads = view.reads();
    fireEvent.click(await screen.findByRole('button', { name: 'Revoke' }));
    await waitFor(() => expect(view.reads()).toBeGreaterThan(reads));
    await waitFor(() => expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Revoke' }).disabled).toBe(false));
    expect(revokes).toHaveLength(press);
  }
  expect(revokes).toEqual(['/api/mobile/devices/gone', '/api/mobile/devices/gone']);
  expect(screen.queryByRole('alert')).toBeNull();
  sessionKept(view);
});

it('a stale Approve shows the server reason in the pane and keeps the session', async () => {
  const reason = 'No pending pairing request with this id';
  const view = mount(() => Promise.resolve(answered(404, 'not_found', reason)));
  fireEvent.click(await screen.findByRole('button', { name: 'Approve 123456' }));
  expect((await screen.findByRole('alert')).textContent).toContain(reason);
  sessionKept(view);
});

it('a write with no answer shows the fixed sentence, never the transport text', async () => {
  const view = mount(() => Promise.reject(new TypeError('Failed to fetch')));
  fireEvent.click(await screen.findByRole('button', { name: 'Revoke' }));
  const alert = await screen.findByRole('alert');
  expect(alert.textContent).toContain(MOBILE_WRITE_TEXT.unknown);
  expect(alert.textContent).not.toMatch(/transport|fetch|connection|offline/i);
  sessionKept(view);
});

it('a second QR whose answer is lost takes the first one off screen: the server may already have replaced it', async () => {
  let creates = 0;
  mount((request) => {
    if (request.path !== '/api/mobile/pairings') return Promise.reject(new Error(`unexpected ${request.path}`));
    creates += 1;
    return creates === 1
      ? Promise.resolve(ok({ id: 'first', qrPayload: 'p', qrImage: 'data:image/svg+xml;base64,AA==', expiresInSeconds: 180 }))
      : Promise.reject(new TypeError('Failed to fetch'));
  });
  fireEvent.click(await screen.findByRole('button', { name: 'Create QR code' }));
  expect(await screen.findByAltText('Scan to pair this Neige workspace')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: 'Create QR code' }));
  expect((await screen.findByRole('alert')).textContent).toContain(MOBILE_WRITE_TEXT.unknown);
  expect(screen.queryByAltText('Scan to pair this Neige workspace')).toBeNull();
  expect(creates).toBe(2);
});
