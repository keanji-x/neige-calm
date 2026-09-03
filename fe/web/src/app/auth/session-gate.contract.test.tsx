// @vitest-environment jsdom
import { QueryClient } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { IDB_DB_NAME, SYNC_CURSOR_KEY } from '../../../../core/keys/storage.ts';
import { createBrowserCursorStore } from '../events/browser-cursor-store.ts';
import { SessionGate } from './session-gate.tsx';
import type { ProviderRuntime } from '../providers/public.tsx';
import { ProductionApp } from './production-app.tsx';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';

afterEach(cleanup);
const identity = { userId: 'u', displayName: 'Owner', role: 'admin', sessionId: 's' };

function cleanupRuntime() {
  return { deleteDatabase: vi.fn(), idbDatabaseName: IDB_DB_NAME };
}

const noopCursorStore = { clear: () => undefined };

describe('session gate contracts', () => {
  it('does not mount the real AppProviders ServerCompatGate before an unauthenticated verdict', async () => {
    const paths: string[] = [];
    const transport: ApiTransportPort = { send: (request) => { paths.push(request.path); return Promise.resolve({ status: 401, statusText: 'Unauthorized', body: {} }); } };
    const client = new QueryClient();
    const fetchVersion = vi.fn();
    const runtime: ProviderRuntime = { fetchVersion, reload: vi.fn(), deleteDatabase: vi.fn(), idbDatabaseName: IDB_DB_NAME, storage: { getItem: () => null, setItem: vi.fn(), removeItem: vi.fn() } };
    const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
    const router = createAppRouter({ transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn() });
    render(<ProductionApp transport={transport} client={client} unauthorized={unauthorized} runtime={runtime}
      cursorStore={{ clear: vi.fn() }} router={router} renderLogin={() => <b>login</b>} renderError={() => <b>retry</b>} />);
    expect(paths).toEqual(['/api/auth/whoami']);
    expect(await screen.findByText('login')).toBeTruthy();
    expect(fetchVersion).not.toHaveBeenCalled();
  });

  it('renders login on 401, error with retry on transport failure, and clears on broadcast 401', async () => {
    const channel = createUnauthorizedChannel({ enqueue: (task) => task() });
    const client = new QueryClient(); client.setQueryData(['private'], 'cached');
    let reply: '401' | 'error' | 'ok' = '401';
    const transport: ApiTransportPort = { send: () => reply === 'error' ? Promise.reject(new Error('offline')) : Promise.resolve(reply === '401' ? { status: 401, statusText: 'Unauthorized', body: {} } : { status: 200, statusText: 'OK', body: identity }) };
    const first = render(<SessionGate transport={transport} client={client} unauthorized={channel}
      runtime={cleanupRuntime()} cursorStore={noopCursorStore}
      renderLogin={() => <b>login</b>} renderError={(retry) => <button onClick={retry}>Try again</button>}><b>router</b></SessionGate>);
    expect(await screen.findByText('login')).toBeTruthy();
    first.unmount(); reply = 'error';
    render(<SessionGate transport={transport} client={client} unauthorized={channel}
      runtime={cleanupRuntime()} cursorStore={noopCursorStore}
      renderLogin={() => <b>login</b>} renderError={(retry) => <button onClick={retry}>Try again</button>}><b>router</b></SessionGate>);
    expect(await screen.findByRole('button', { name: 'Try again' })).toBeTruthy();
    reply = 'ok'; await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    expect(await screen.findByText('router')).toBeTruthy();
    client.setQueryData(['private'], 'cached'); channel.notify();
    await waitFor(() => expect(screen.getByText('login')).toBeTruthy());
    expect(client.getQueryData(['private'])).toBeUndefined();
  });

  it('CAP-APP-064 clears query data, the sync cursor, and IndexedDB in order before rendering login', async () => {
    const channel = createUnauthorizedChannel({ enqueue: (task) => task() });
    const sequence: string[] = [];
    const values = new Map<string, string>([[SYNC_CURSOR_KEY, JSON.stringify({ dbInstanceId: 'db-a', cursor: 41 })]]);
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value); },
      removeItem: (key: string) => { values.delete(key); },
    };
    const browserCursorStore = createBrowserCursorStore(storage);
    const cursorStore = { clear: () => { sequence.push('cursor'); browserCursorStore.clear(); } };
    const client = new QueryClient();
    client.setQueryData(['private'], 'cached');
    const clear = client.clear.bind(client);
    vi.spyOn(client, 'clear').mockImplementation(() => { sequence.push('query'); clear(); });
    const runtime = {
      idbDatabaseName: IDB_DB_NAME,
      deleteDatabase: (name: string) => { sequence.push(`indexed-db:${name}`); },
    };
    const transport: ApiTransportPort = { send: () => Promise.resolve({ status: 200, statusText: 'OK', body: identity }) };

    render(<SessionGate transport={transport} client={client} unauthorized={channel}
      runtime={runtime} cursorStore={cursorStore}
      renderLogin={() => { sequence.push('unauthed'); return <b>login</b>; }} renderError={() => <b>retry</b>}><b>router</b></SessionGate>);
    expect(await screen.findByText('router')).toBeTruthy();
    channel.notify();

    expect(await screen.findByText('login')).toBeTruthy();
    expect(client.getQueryData(['private'])).toBeUndefined();
    expect(values.get(SYNC_CURSOR_KEY)).toBeUndefined();
    expect(sequence).toEqual(['query', 'cursor', `indexed-db:${IDB_DB_NAME}`, 'unauthed']);
  });

  it('clears session artifacts before rendering login for an initial unauthenticated verdict', async () => {
    const sequence: string[] = [];
    const client = new QueryClient();
    client.setQueryData(['private'], 'cached');
    const clear = client.clear.bind(client);
    vi.spyOn(client, 'clear').mockImplementation(() => { sequence.push('query'); clear(); });
    const transport: ApiTransportPort = {
      send: () => Promise.resolve({ status: 401, statusText: 'Unauthorized', body: {} }),
    };

    render(<SessionGate transport={transport} client={client}
      unauthorized={createUnauthorizedChannel({ enqueue: (task) => task() })}
      runtime={{ idbDatabaseName: IDB_DB_NAME, deleteDatabase: () => { sequence.push('indexed-db'); } }}
      cursorStore={{ clear: () => { sequence.push('cursor'); } }}
      renderLogin={() => { sequence.push('unauthed'); return <b>login</b>; }} renderError={() => <b>retry</b>}><b>router</b></SessionGate>);

    expect(await screen.findByText('login')).toBeTruthy();
    expect(client.getQueryData(['private'])).toBeUndefined();
    expect(sequence).toEqual(['query', 'cursor', 'indexed-db', 'unauthed']);
  });

  it('keeps the 401 verdict when an older successful whoami resolves later', async () => {
    let resolveWhoami!: (value: { status: 200; statusText: string; body: typeof identity }) => void;
    const transport: ApiTransportPort = { send: () => new Promise((resolve) => { resolveWhoami = resolve; }) };
    const channel = createUnauthorizedChannel({ enqueue: (task) => task() });
    render(<SessionGate transport={transport} client={new QueryClient()} unauthorized={channel}
      runtime={cleanupRuntime()} cursorStore={noopCursorStore}
      renderLogin={() => <b>login</b>} renderError={() => <b>retry</b>}><b>router</b></SessionGate>);
    channel.notify();
    expect(await screen.findByText('login')).toBeTruthy();
    resolveWhoami({ status: 200, statusText: 'OK', body: identity });
    await waitFor(() => expect(screen.queryByText('router')).toBeNull());
    expect(screen.getByText('login')).toBeTruthy();
  });
});
