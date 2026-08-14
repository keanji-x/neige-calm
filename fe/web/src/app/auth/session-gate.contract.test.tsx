// @vitest-environment jsdom
import { QueryClient } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { SessionGate } from './session-gate.tsx';
import { AppProviders, type ProviderRuntime } from '../providers/public.tsx';

afterEach(cleanup);
const identity = { userId: 'u', displayName: 'Owner', role: 'admin', sessionId: 's' };

describe('session gate contracts', () => {
  it('does not mount the real AppProviders ServerCompatGate before an unauthenticated verdict', async () => {
    const paths: string[] = [];
    const transport: ApiTransportPort = { send: (request) => { paths.push(request.path); return Promise.resolve({ status: 401, statusText: 'Unauthorized', body: {} }); } };
    const client = new QueryClient();
    const runtime: ProviderRuntime = { fetchVersion: vi.fn(), reload: vi.fn(), deleteDatabase: vi.fn(), idbDatabaseName: 'calm', storage: localStorage };
    render(<SessionGate transport={transport} client={client} unauthorized={createUnauthorizedChannel({ enqueue: (task) => task() })} renderLogin={() => <b>login</b>} renderError={() => <b>retry</b>}><AppProviders client={client} runtime={runtime} cursorStore={{ clear: vi.fn() }}><b>router</b></AppProviders></SessionGate>);
    expect(paths).toEqual(['/api/auth/whoami']);
    expect(await screen.findByText('login')).toBeTruthy();
    expect(runtime.fetchVersion).not.toHaveBeenCalled();
  });

  it('renders login on 401, error with retry on transport failure, and clears on broadcast 401', async () => {
    const channel = createUnauthorizedChannel({ enqueue: (task) => task() });
    const client = new QueryClient(); client.setQueryData(['private'], 'cached');
    let reply: '401' | 'error' | 'ok' = '401';
    const transport: ApiTransportPort = { send: () => reply === 'error' ? Promise.reject(new Error('offline')) : Promise.resolve(reply === '401' ? { status: 401, statusText: 'Unauthorized', body: {} } : { status: 200, statusText: 'OK', body: identity }) };
    const first = render(<SessionGate transport={transport} client={client} unauthorized={channel} renderLogin={() => <b>login</b>} renderError={(retry) => <button onClick={retry}>Try again</button>}><b>router</b></SessionGate>);
    expect(await screen.findByText('login')).toBeTruthy();
    first.unmount(); reply = 'error';
    render(<SessionGate transport={transport} client={client} unauthorized={channel} renderLogin={() => <b>login</b>} renderError={(retry) => <button onClick={retry}>Try again</button>}><b>router</b></SessionGate>);
    expect(await screen.findByRole('button', { name: 'Try again' })).toBeTruthy();
    reply = 'ok'; await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    expect(await screen.findByText('router')).toBeTruthy();
    client.setQueryData(['private'], 'cached'); channel.notify();
    await waitFor(() => expect(screen.getByText('login')).toBeTruthy());
    expect(client.getQueryData(['private'])).toBeUndefined();
  });

  it('keeps the 401 verdict when an older successful whoami resolves later', async () => {
    let resolveWhoami!: (value: { status: 200; statusText: string; body: typeof identity }) => void;
    const transport: ApiTransportPort = { send: () => new Promise((resolve) => { resolveWhoami = resolve; }) };
    const channel = createUnauthorizedChannel({ enqueue: (task) => task() });
    render(<SessionGate transport={transport} client={new QueryClient()} unauthorized={channel} renderLogin={() => <b>login</b>} renderError={() => <b>retry</b>}><b>router</b></SessionGate>);
    channel.notify();
    expect(await screen.findByText('login')).toBeTruthy();
    resolveWhoami({ status: 200, statusText: 'OK', body: identity });
    await waitFor(() => expect(screen.queryByText('router')).toBeNull());
    expect(screen.getByText('login')).toBeTruthy();
  });
});
