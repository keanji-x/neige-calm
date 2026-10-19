import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import '../../styles/entry.css';
import { QueryClient } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, within } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { AppProviders, WEB_COMPAT_VERSION } from './public.tsx';
import { createReadReceiptsKernel, type ReadReceiptsKernel } from './read-receipts-fixture.tsx';
import { createUiPreferences } from './ui-preferences.tsx';

/* #2596: read state is the kernel's. Each `load` is one page load of the production provider stack
   (compat gate, router, shell) over one browser profile's storage; the kernel outlives every load. */

afterEach(() => { cleanup(); });

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const area = { id: 'work', name: 'Work', color: '#6574cd', sort: 0, kind: 'user', created_at: 1, updated_at: 1 };
const row = (id: string, title: string) => ({ id, area_id: area.id, title, sort: 0, cwd: '/tmp',
  workspace: { isolation: 'worktree', kind: 'attached', path: '/tmp', frozen_at: null, worktree: `/tmp/.worktrees/${id}` },
  pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 });
const tracks = [row('seen', 'Seen'), row('fresh', 'Fresh')];
const activity = (id: string, at: number) => ({ id: `activity-${id}`, plugin_id: 'kernel', entity_kind: 'track', entity_id: id,
  kind: 'activity', payload: { schemaVersion: 4, working: false, attention: 'none', activity_at_ms: at, items: [], cards: [] }, updated_at: at });
const overlays = [activity('seen', 100), activity('fresh', 200)];

function load(kernel: ReadReceiptsKernel, storage: Map<string, string>, initial: string) {
  const browserStorage = {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => { storage.set(key, value); },
    removeItem: (key: string) => { storage.delete(key); },
  };
  const transport: ApiTransportPort = { send: async (request) => {
    await Promise.resolve();
    const receipt = await kernel.reply(request);
    if (receipt !== undefined) return receipt;
    if (request.path === '/api/areas') return ok([area]);
    if (request.path === '/api/areas/work/tracks') return ok(tracks);
    if (request.path.startsWith('/api/overlays?')) return ok(overlays);
    const detail = tracks.find((track) => request.path === `/api/tracks/${track.id}`);
    if (detail) return ok({ track: detail, can_reopen: false, can_admin: false, can_close: true, cards: [],
      overlays: overlays.filter((overlay) => overlay.entity_id === detail.id) });
    return ok([]);
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, unauthorized, client, cards: bootTestCardRuntime(),
    uiPreferences: createUiPreferences(browserStorage), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [initial] }) });
  const runtime = { storage: browserStorage, reload: () => undefined, deleteDatabase: () => undefined, idbDatabaseName: 'test',
    fetchVersion: () => Promise.resolve({ webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
      syncEventVersion: 20, dbInstanceId: 'boot-1', databaseId: 'db-1' }) };
  const container = document.body.appendChild(document.createElement('div'));
  const view = render(<AppProviders client={client} runtime={runtime} cursorStore={{ clear() {} }}>
    <RouterProvider router={router} />
  </AppProviders>, { container });
  /** The rail row's activity marker: `unread`, another state, or `null` for none. */
  const marker = (title: string) => [...container.querySelectorAll('nav [aria-label^="Track "]')]
    .find((row) => row.getAttribute('aria-label')?.startsWith(`Track ${title}`))
    ?.parentElement?.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity') ?? null;
  return { view, marker, container };
}

const readKeys = (storage: Map<string, string>) => [...storage.keys()].filter((key) => /read(%3A|:)/.test(key));

it('keeps a visited track read after a page refresh, with nothing kept in browser storage', async () => {
  await page.viewport(1400, 900);
  const kernel = createReadReceiptsKernel({ baselineMs: 50 });
  const storage = new Map<string, string>();
  const first = load(kernel, storage, '/track/seen');
  await expect.poll(() => first.marker('Fresh')).toBe('unread');
  await expect.poll(() => kernel.stored('track', 'seen')?.readThroughMs).toBe(100);
  await expect.poll(() => within(first.container)
    .getByRole('button', { name: 'Track Seen' })
    .parentElement?.querySelector('[data-nc-activity]')).toBeNull();
  first.view.unmount();

  const second = load(kernel, storage, '/');
  await expect.poll(() => second.marker('Fresh')).toBe('unread');
  await expect.poll(() => within(second.container)
    .getByRole('button', { name: 'Track Seen' })
    .parentElement?.querySelector('[data-nc-activity]')).toBeNull();
  expect(readKeys(storage)).toEqual([]);
});

it('keeps a manual unread after a page refresh', async () => {
  await page.viewport(1400, 900);
  const kernel = createReadReceiptsKernel({ baselineMs: 50,
    receipts: [{ kind: 'track', targetId: 'seen', readThroughMs: 100, manualUnread: false }] });
  const storage = new Map<string, string>();
  const first = load(kernel, storage, '/');
  await expect.poll(() => first.marker('Fresh')).toBe('unread');
  await expect.poll(() => within(first.container)
    .getByRole('button', { name: 'Track Seen' })
    .parentElement?.querySelector('[data-nc-activity]')).toBeNull();
  await page.getByRole('button', { name: 'Track Seen', exact: true }).hover();
  await page.getByRole('button', { name: 'Actions for track Seen', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Mark as unread', exact: true }).click();
  await expect.poll(() => kernel.stored('track', 'seen')?.manualUnread).toBe(true);
  first.view.unmount();

  const second = load(kernel, storage, '/');
  await expect.poll(() => second.marker('Fresh')).toBe('unread');
  await expect.poll(() => second.marker('Seen')).toBe('unread');
  expect(readKeys(storage)).toEqual([]);
});

it('shows a second browser profile the same state once it is focused again', async () => {
  await page.viewport(1400, 900);
  const kernel = createReadReceiptsKernel({ baselineMs: 50 });
  const desktop = load(kernel, new Map(), '/');
  const phone = load(kernel, new Map(), '/');
  await expect.poll(() => desktop.marker('Fresh')).toBe('unread');
  await expect.poll(() => phone.marker('Fresh')).toBe('unread');
  await expect.poll(() => phone.marker('Seen')).toBe('unread');

  // The desktop opens Seen; the phone, still in the background, shows what it last read.
  await page.elementLocator(desktop.container).getByRole('button', { name: 'Track Seen', exact: true }).click();
  await expect.poll(() => kernel.stored('track', 'seen')?.readThroughMs).toBe(100);
  await expect.poll(() => within(desktop.container)
    .getByRole('button', { name: 'Track Seen' })
    .parentElement?.querySelector('[data-nc-activity]')).toBeNull();
  expect(phone.marker('Seen')).toBe('unread');
  window.dispatchEvent(new Event('visibilitychange'));
  await expect.poll(() => within(phone.container)
    .getByRole('button', { name: 'Track Seen' })
    .parentElement?.querySelector('[data-nc-activity]')).toBeNull();
  expect(phone.marker('Fresh')).toBe('unread');
});
