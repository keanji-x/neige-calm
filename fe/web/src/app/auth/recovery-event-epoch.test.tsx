import { Component, type ReactNode } from 'react';
import { QueryClient } from '@tanstack/react-query';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { RecoverySession } from '../../systems/recovery/session.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { createRecoveryUnauthorizedChannel } from '../../systems/recovery/unauthorized.ts';
import { createEventComposition } from '../composition.ts';
import { RecoveryEventBridge } from '../events/recovery-event-bridge.tsx';
import { createBrowserCursorStore } from '../events/browser-cursor-store.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { RecoveryGate } from './recovery-gate.tsx';

class Boundary extends Component<{ children: ReactNode }, { error: string | null }> {
  state = { error: null as string | null };
  static getDerivedStateFromError(error: Error) { return { error: error.message }; }
  render() { return this.state.error ? <p role="alert">{this.state.error}</p> : this.props.children; }
}
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it.each([
  { syncEventVersion: 19, transition: 'resume' }, { syncEventVersion: 20, transition: 'resume' },
  { syncEventVersion: 19, transition: 'login' }, { syncEventVersion: 20, transition: 'login' },
])('keeps the recovery workspace after $transition with compatible event version $syncEventVersion', async ({ syncEventVersion, transition }) => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const access = new RecoveryAccess();
  const client = new QueryClient();
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); } };
  const transport = createRecoveryTransports({ send: vi.fn() }, access).business;
  const unauthorized = createRecoveryUnauthorizedChannel(access, { enqueue: queueMicrotask });
  const cursorStore = createBrowserCursorStore(storage);
  const sockets: { closed: boolean }[] = []; let peak = 0;
  const createEvents = () => createEventComposition({ storage, transport, cursorStore, probeUnauthorized: () => Promise.resolve(),
    socketFactory: () => {
      const socket = { closed: false, onopen: null, onclose: null, onmessage: null, onerror: null,
        send: vi.fn(), close: () => { socket.closed = true; } };
      sockets.push(socket); peak = Math.max(peak, sockets.filter(socket => !socket.closed).length);
      return socket;
    } });
  let sessionId = 'first-session';
  let version = { webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 19, dbInstanceId: 'old-db' };
  const session = new RecoverySession({ access, storage, origin: window.location.origin, compatibleVersion: WEB_COMPAT_VERSION,
    identity: () => Promise.resolve({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId }),
    version: () => Promise.resolve(version), logout: () => Promise.resolve(),
    clear: () => { client.clear(); cursorStore.clear(); }, adoptScope: () => {}, online: () => true, visible: () => true });
  render(<Boundary><RecoveryGate client={client} transport={transport} unauthorized={unauthorized} recovery={session}
    runtime={{ storage, fetchVersion: () => Promise.resolve(version), reload: vi.fn(), deleteDatabase: vi.fn(), idbDatabaseName: 'review' }}
    cursorStore={cursorStore} renderLogin={() => <p>Login</p>}
    renderEventBridge={server => <RecoveryEventBridge key={`${session.scopeRevision}:${server.syncEventVersion}`}
      createEvents={createEvents} recovery={session} client={client} version={server} />}>
    <p>Authorized workspace</p>
  </RecoveryGate></Boundary>);
  await screen.findByText('Authorized workspace');
  if (transition === 'login') await act(() => session.signOut());
  else act(() => session.pause());
  expect(document.querySelector('[data-nc-event-bridge]')).toBeNull();
  expect(sockets.every(socket => socket.closed)).toBe(true);
  version = { ...version, syncEventVersion, dbInstanceId: 'new-db' };
  sessionId = 'next-session';
  if (transition === 'login') await act(() => session.verifyNewSession());
  else act(() => session.resume());
  await waitFor(() => expect(session.version?.dbInstanceId).toBe('new-db'));
  expect(screen.queryByRole('alert')?.textContent ?? null).toBeNull();
  expect(screen.queryByText('Authorized workspace')).not.toBeNull();
  expect(sockets.filter(socket => !socket.closed)).toHaveLength(1);
  expect(peak).toBe(1);
});
