import { StrictMode } from 'react';
import { QueryClient, onlineManager } from '@tanstack/react-query';
import { createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor, act } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import type { SessionIdentity } from '../../../../core/api/auth.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { RecoverySession } from '../../systems/recovery/session.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { createRecoveryUnauthorizedChannel } from '../../systems/recovery/unauthorized.ts';
import { createIsolatedRetryFixture } from '../router/isolated-task-retry-fixture.tsx';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { ProductionApp } from './production-app.tsx';
import { createEventComposition } from '../composition.ts';
import { EventBridge } from '../events/event-bridge.tsx';
import type { SocketPort } from '../../systems/events/websocket-driver.ts';
import { queryKeys } from '../providers/queries.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
it.each(['before', 'after'] as const)('cold saved Track and hot reconnect retain the production page with Query online listener %s lifecycle', async (listenerOrder) => {
  vi.stubGlobal('__NC_BUNDLED__', true); await page.viewport(390, 844);
  window.history.replaceState({}, '', '/next/track/w1');
  const fixture = createIsolatedRetryFixture(); const access = new RecoveryAccess();
  const transport = createRecoveryTransports(fixture.transport, access).business;
  const unauthorized = createRecoveryUnauthorizedChannel(access, { enqueue: task => queueMicrotask(task) });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); } };
  let finish!: (identity: SessionIdentity) => void;
  const whoami = vi.fn(() => new Promise<SessionIdentity>(resolve => { finish = resolve; }));
  const version = { webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 3, dbInstanceId: 'phone-test' };
  const fetchVersion = vi.fn(() => Promise.resolve(version));
  const recovery = new RecoverySession({ access, storage, origin: window.location.origin, compatibleVersion: WEB_COMPAT_VERSION, adoptScope: vi.fn(),
    identity: whoami, version: fetchVersion, logout: () => Promise.resolve(), clear: () => client.clear(), online: () => true, visible: () => true });
  const sockets: SocketPort[] = [];
  const events = createEventComposition({ storage, transport, probeUnauthorized: () => { recovery.resume(); return Promise.resolve(); },
    socketFactory: () => {
      const socket: SocketPort = { onopen: null, onmessage: null, onclose: null, onerror: null, send: vi.fn(), close: vi.fn() };
      sockets.push(socket); return socket;
    },
  });
  events.stream.onConnectionState(state => recovery.events(state));
  const replay = () => {
    const socket = sockets.at(-1)!; socket.onopen?.(new Event('open'));
    socket.onmessage?.(new MessageEvent('message', { data: JSON.stringify({ ev: '_replay_complete', _id: 0 }) }));
  };
  const painted = () => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
  const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: () => { void recovery.signOut(); } });
  router.update({ history: createMemoryHistory({ initialEntries: ['/track/w1'] }) });
  const mounted = render(<StrictMode><ProductionApp transport={transport} unauthorized={unauthorized} client={client} router={router} recovery={recovery}
    runtime={{ fetchVersion, reload: vi.fn(), deleteDatabase: vi.fn(), idbDatabaseName: 'phone', storage }}
    cursorStore={events.store} renderEventBridge={server => <EventBridge client={client} stream={events.stream} cursor={events.store} syncEventVersion={server.syncEventVersion} dbInstanceId={server.dbInstanceId} />} renderLogin={() => <p>Sign in</p>} renderError={() => <p>Network failure</p>} /></StrictMode>);
  expect(screen.getByRole('heading', { name: 'Track' })).toBeTruthy(); expect(fetchVersion).not.toHaveBeenCalled();
  expect(fixture.requests).toHaveLength(0);
  await page.screenshot({ path: '../../../../test-results/1712-cold-track-390.png' });
  act(() => finish({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 's' }));
  await screen.findByText(fixture.goal);
  act(replay); await waitFor(() => expect(access.read().phase).toBe('connected')); await painted();
  const track = document.querySelector('[data-nc-track-page]')!; expect(track).not.toBeNull();
  if (listenerOrder === 'after') { client.unmount(); client.mount(); }
  act(() => { recovery.resume(); window.dispatchEvent(new Event('online')); });
  await act(() => client.refetchQueries({ queryKey: queryKeys.trackDetail('w1'), type: 'active' }));
  expect(client.getQueryState(queryKeys.trackDetail('w1'))?.status).toBe('success');
  expect(client.getQueryState(queryKeys.trackDetail('w1'))?.fetchStatus).toBe('paused');
  expect(document.querySelector('[data-nc-track-page]')).toBe(track);
  expect(screen.getByText(fixture.goal)).toBeTruthy(); expect(() => access.capture()).toThrow();
  await page.screenshot({ path: '../../../../test-results/1712-hot-retry-390.png' });
  act(() => finish({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 's' }));
  await waitFor(() => expect(access.read().phase).toBe('syncing')); act(replay); await waitFor(() => expect(access.read().phase).toBe('connected')); await painted();
  expect(document.querySelector('[data-nc-track-page]')).toBe(track);
  expect(screen.getByText('已连接')).toBeTruthy();
  const status = document.querySelector('[data-nc-recovery-status]')!.getBoundingClientRect();
  expect(status.right).toBeLessThanOrEqual(390);
  expect(status.top).toBeGreaterThanOrEqual(document.querySelector('[data-nc-mobile-header]')!.getBoundingClientRect().bottom);
  await page.screenshot({ path: '../../../../test-results/1712-restored-track-390.png' });
  mounted.unmount();
  // A subsequent ordinary web client gets the browser's real event source.
  const release = onlineManager.subscribe(() => {});
  window.dispatchEvent(new Event('offline')); expect(onlineManager.isOnline()).toBe(false);
  window.dispatchEvent(new Event('online')); expect(onlineManager.isOnline()).toBe(true);
  release();
});
