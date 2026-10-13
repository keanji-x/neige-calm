// #2596 fix round 2: in the bundled app a receipt write is admitted only while the recovered workspace is connected.
import { QueryClient } from '@tanstack/react-query';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { Area } from '../../../../core/domain/area.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { RecoverySession } from '../../systems/recovery/session.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { RecoveryGate } from '../auth/recovery-gate.tsx';
import { Sidebar } from '../shell/sidebar.tsx';
import { WEB_COMPAT_VERSION } from './public.tsx';
import { ReadReceiptsProvider, useReadReceipt, useReadReceipts } from './read-receipts.tsx';
import { createReadReceiptsKernel, type ReadReceiptsKernel } from './read-receipts-fixture.tsx';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

/** The bundled assembly: a real recovery session and gate over the kernel fake's business transport. */
function bundled(kernel: ReadReceiptsKernel, children: ReactNode) {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); } };
  const access = new RecoveryAccess();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const version = { webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
    syncEventVersion: 20, dbInstanceId: 'boot', databaseId: 'db' };
  const fetchVersion = () => Promise.resolve(version);
  const session = new RecoverySession({ access, storage, origin: 'https://server.test', compatibleVersion: WEB_COMPAT_VERSION,
    version: fetchVersion, identity: () => Promise.resolve({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'session' }),
    adoptScope: () => undefined, logout: () => Promise.resolve(), clear: () => undefined,
    online: () => true, visible: () => !document.hidden });
  const business = createRecoveryTransports(kernel.transport, access).business;
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const tree = (node: ReactNode) => <RecoveryGate recovery={session} transport={business} unauthorized={unauthorized} client={client}
    runtime={{ storage, fetchVersion, reload() {}, deleteDatabase() {}, idbDatabaseName: 'test' }}
    cursorStore={{ clear() {} }} renderLogin={() => <p>Login</p>}>
    <ReadReceiptsProvider transport={business} unauthorized={unauthorized}>{node}</ReadReceiptsProvider>
  </RecoveryGate>;
  const view = render(tree(children));
  /* `connected` is what the event bridge sets once its replay completes; the gate mounts no bridge here. */
  const connect = async () => {
    await waitFor(() => expect(access.read().phase).toBe('syncing'));
    act(() => access.change('connected'));
  };
  return { access, connect, rerender: (node: ReactNode) => view.rerender(tree(node)) };
}

function Open({ at }: { at: number }) {
  useReadReceipt('track', 't', at);
  const receipts = useReadReceipts();
  return <p>t: {receipts.ready ? (receipts.isUnread('track', 't', at) ? 'unread' : 'read') : 'pending'}</p>;
}

function setVisible(visible: boolean) {
  vi.spyOn(document, 'hidden', 'get').mockReturnValue(!visible);
  vi.spyOn(document, 'visibilityState', 'get').mockReturnValue(visible ? 'visible' : 'hidden');
  act(() => { document.dispatchEvent(new Event('visibilitychange')); });
}

it('acknowledges the open target on a bundled cold start once the workspace is connected', async () => {
  const kernel = createReadReceiptsKernel({ baselineMs: 100 });
  const app = bundled(kernel, <Open at={150} />);
  await screen.findByText('t: unread');
  expect(kernel.posts()).toEqual([]);
  await app.connect();
  await waitFor(() => expect(kernel.posts().map((request) => [request.path, request.body])).toEqual([
    ['/api/read-receipts/track/t/read', { readThroughMs: 150 }],
  ]));
  await screen.findByText('t: read');
});

it('acknowledges the open target once a bundled resume admits writes again', async () => {
  const kernel = createReadReceiptsKernel({ baselineMs: 100,
    receipts: [{ kind: 'track', targetId: 't', readThroughMs: 150, manualUnread: false }] });
  const app = bundled(kernel, <Open at={150} />);
  await app.connect();
  await screen.findByText('t: read');
  await new Promise((resolve) => setTimeout(resolve, 50));
  const before = kernel.posts().length;
  // Hidden: the session pauses, and the activity that lands meanwhile is not acknowledged.
  setVisible(false);
  await waitFor(() => expect(app.access.read().phase).toBe('paused'));
  app.rerender(<Open at={200} />);
  // Shown again: recovering, then syncing; the tab is visible but no write is admitted yet.
  setVisible(true);
  await waitFor(() => expect(app.access.read().phase).toBe('syncing'));
  expect(kernel.posts()).toHaveLength(before);
  await app.connect();
  await waitFor(() => expect(kernel.posts().slice(before).map((request) => request.body)).toEqual([{ readThroughMs: 200 }]));
  await screen.findByText('t: read');
});

it('says a mark unread refused during recovery like any other write, and changes nothing', async () => {
  const area: Area = { id: 'work', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0 };
  const track: Track = { id: 't', areaId: 'work', title: 'Task', sort: 1, cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 0, updatedAt: 0, ...NEUTRAL_ACTIVITY, activityAt: 50 };
  const kernel = createReadReceiptsKernel({ baselineMs: 100 });
  bundled(kernel, <Sidebar areas={[area]} tracksByArea={new Map([['work', [track]]])} tracks={[track]} currentPath="/"
    onGo={vi.fn()} onRequestCreateArea={vi.fn()} onRequestEditArea={vi.fn()} onDeleteArea={vi.fn()} onNewTrack={vi.fn()}
    onSetPinned={vi.fn()} onDeleteTrack={vi.fn()} collapsed={false} onToggleCollapsed={vi.fn()}
    onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()} />);
  await screen.findByRole('button', { name: 'Actions for track Task' });
  await waitFor(() => expect(kernel.requests).toHaveLength(1));
  await userEvent.click(screen.getByRole('button', { name: 'Actions for track Task' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'Mark as unread' }));
  expect(await screen.findByText('The track was not marked unread.')).toBeTruthy();
  expect(kernel.posts()).toEqual([]);
  // Nothing flipped: no row of the track reads unread.
  expect(screen.getAllByRole('button', { name: /^Track Task/ }).every((row) => row.getAttribute('aria-describedby') === null)).toBe(true);
});
