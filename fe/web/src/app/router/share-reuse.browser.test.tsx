import '../../styles/entry.css';
import { act, cleanup, waitFor } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import type { ApiRequest } from '../../../../core/api/types.ts';
import { DB_INSTANCE_ID_KEY } from '../../../../core/keys/storage.ts';
import { mountProductionApp } from '../auth/production-app.tsx';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { createTrackTaskFixture } from './track-task-fixture.ts';
import { createMemoryStorage } from './share-reuse-fixture.ts';
import { historicalConversationRun, historicalConversationPage } from './conversation-read-data.ts';

const lifecycle = vi.hoisted(() => ({ unmount: undefined as (() => void) | undefined }));
vi.mock('react-dom/client', async original => {
  const actual = await original<typeof import('react-dom/client')>();
  return { ...actual, createRoot: (...args: Parameters<typeof actual.createRoot>) => {
    const root = actual.createRoot(...args);
    lifecycle.unmount = () => root.unmount();
    return root;
  } };
});
afterEach(() => {
  lifecycle.unmount?.(); lifecycle.unmount = undefined; cleanup(); document.getElementById('root')?.remove();
  vi.unstubAllGlobals();
  window.history.replaceState({}, '', '/');
});
function mountShare(kind: 'track' | 'area', unlocked = false, options: Readonly<{
  hash?: string; whoami?: () => Promise<Response>; onProbe?: () => void;
}> = {}) {
  const fixture = createTrackTaskFixture();
  const requests: ApiRequest[] = [];
  const sockets: string[] = [];
  let authorized = unlocked;
  let archivedAt: number | null = null;
  const scope = kind === 'track' ? { kind, id: 'w1' } : { kind, id: 'c1' };
  vi.stubGlobal('WebSocket', class {
    static CONNECTING = 0; static OPEN = 1;
    readyState = 0; onopen = null; onmessage = null; onclose = null; onerror = null;
    constructor(url: string) { sockets.push(url); }
    close() {} send() {}
  });
  vi.stubGlobal('fetch', async (url: string, init: RequestInit) => {
    const request: ApiRequest = { path: url, method: init.method as ApiRequest['method'], credentials: 'include',
      headers: init.headers as Record<string, string>, ...(init.body ? { body: JSON.parse(init.body as string) } : {}) };
    requests.push(request);
    const reply = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
    if (url === '/api/share-links/link/access') return reply(authorized ? { status: 'unlocked', scope, expiresAt: Date.now() + 86400000 } : { status: 'locked', expiresAt: Date.now() + 86400000 });
    if (url === '/api/share-links/link/unlock') { authorized = true; return reply({ scope, expiresAt: Date.now() + 86400000 }); }
    if (url === '/api/auth/whoami') {
      options.onProbe?.();
      if (options.whoami) return options.whoami();
      return authorized ? reply({ userId: 'local-owner', displayName: '访客', role: 'owner', sessionId: 'guest-session' }) : reply({}, 401);
    }
    if (url === '/api/tracks/w1/conversations') return reply([{ id: 'assistant', trackId: 'w1', title: 'Shared Assistant', kind: 'track-assistant',
      provider: 'codex', state: null, updatedAt: 2, archivedAt, lastTurnCompletedAt: null }]);
    if (url === '/api/tracks/w1/conversations/assistant/archive') { archivedAt = Date.now(); return new Response(null, { status: 204 }); }
    if (url === '/api/version') return reply({ webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 3, dbInstanceId: 'guest-instance' });
    if (url === '/api/tracks/w1') {
      const result = await fixture.transport.send(request);
      const detail = result.body as { cards: unknown[] };
      return reply({ ...detail, cards: [...detail.cards, { id: 'planner', track_id: 'w1', kind: 'codex', title: 'Planner', sort: 2,
        payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 }] });
    }
    if (url.includes('/planner/run?')) return reply(historicalConversationRun({ card_id: 'planner', worker_session_id: null, phase: 'idle', model: null,
      reasoning_effort: null, permission_mode: 'never', blocked_reason: null, manual_compact_supported: true, recovery: null,
      compact_outcome: null, context_usage_invalidated: false, running_turn: null, attachments_supported: true }, null, true));
    if (url.includes('/harness/items?')) return reply(historicalConversationPage('planner', [], null));
    const result = await fixture.transport.send(request);
    return reply(result.body, result.status);
  });
  const storage = createMemoryStorage();
  storage.setItem(DB_INSTANCE_ID_KEY, 'owner-instance');
  window.history.replaceState({ preserved: true }, '', `/next/share/link/track/w1${options.hash ?? ''}`);
  const root = document.createElement('div'); root.id = 'root'; document.body.append(root);
  act(() => { mountProductionApp(root, { storage, submittedStorage: createMemoryStorage,
    reload: vi.fn(), deleteDatabase: vi.fn() }); });
  return { requests, sockets, fixture, storage };
}
it('unlocks into the production Report, tasks, tools and full Planner without a reload', async () => {
  await page.viewport(1440, 1000);
  const { requests, sockets, fixture, storage } = mountShare('track');
  await expect.element(page.getByLabelText('口令')).toBeVisible();
  expect(requests.some(r => r.path === '/api/tracks/w1')).toBe(false);
  await page.getByLabelText('口令').fill('one-time-secret');
  await page.getByRole('button', { name: '解锁', exact: true }).click();
  await page.getByText('Reference', { exact: true }).click();
  await waitFor(() => expect(document.querySelector('[data-nc-task-state] > summary')).not.toBeNull());
  await userEvent.click(document.querySelector('[data-nc-task-state] > summary')!);
  await expect.element(page.getByText(fixture.goal, { exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: /Conversation Planner/ })).toBeVisible();
  expect(document.querySelector('[data-nc-event-bridge]')).not.toBeNull();
  expect(page.getByRole('navigation', { name: 'Workspace' }).query()).toBeNull();
  expect(page.getByText('分享链接', { exact: true }).query()).toBeNull();
  await page.getByRole('button', { name: 'Actions for conversation: Shared Assistant', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Archive conversation', exact: true }).click();
  await waitFor(() => expect(requests.some(r => r.path.endsWith('/conversations/assistant/archive') && (r.body as { archived: boolean }).archived)).toBe(true));
  await page.screenshot({ path: '../../../../test-results/share-reuse-desktop.png' });
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  await expect.element(page.getByRole('combobox', { name: 'Message', exact: true })).toBeVisible();
  expect(window.location.pathname).toBe('/next/share/link/track/w1');
  expect(requests.filter(r => !r.path.startsWith('/api/share-links/')).every(r => r.headers?.['X-Calm-Share-Link'] === 'link')).toBe(true);
  expect(sockets.some(url => url.includes('/api/events?share_link=link'))).toBe(true);
  expect(storage.getItem(DB_INSTANCE_ID_KEY)).toBe('owner-instance');
  expect(storage.getItem(`calm-share:link:${DB_INSTANCE_ID_KEY}`)).toBe('guest-instance');
});
it.each([1280, 390])('limits Area navigation to its Tracks and New Track, with scoped production routing (%ipx)', async width => {
  await page.viewport(width, 900); mountShare('area', true);
  if (width < 600) await page.getByRole('button', { name: 'Open workspace', exact: true }).click();
  await expect.element(page.getByRole('button', { name: '+ New Track', exact: true })).toBeVisible();
  expect(page.getByRole('button', { name: 'Go to Today' }).query()).toBeNull();
  expect(page.getByRole('button', { name: /Account menu/ }).query()).toBeNull();
  expect(page.getByRole('button', { name: 'New area', exact: true }).query()).toBeNull();
  await page.getByRole('button', { name: 'Actions for track The calculation', exact: true }).click();
  expect(page.getByRole('menuitem', { name: 'Share track', exact: true }).query()).toBeNull();
  expect(page.getByRole('menuitem', { name: 'Delete track', exact: true }).query()).toBeNull();
  await userEvent.keyboard('{Escape}');
  await page.getByRole('button', { name: '+ New Track', exact: true }).click();
  await waitFor(() => expect(window.location.pathname).toBe('/next/share/link/area/c1/new'));
});
it('keeps the track share mobile surface inside the production shell without workspace switching', async () => {
  await page.viewport(390, 844); const { fixture } = mountShare('track', true);
  await page.getByText('Reference', { exact: true }).click();
  await waitFor(() => expect(document.querySelector('[data-nc-task-state] > summary')).not.toBeNull());
  await userEvent.click(document.querySelector('[data-nc-task-state] > summary')!);
  await expect.element(page.getByText(fixture.goal, { exact: true })).toBeVisible();
  expect(page.getByRole('button', { name: 'Open workspace', exact: true }).query()).toBeNull();
  await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
  await expect.element(page.getByRole('button', { name: '发起新对话', exact: true })).toBeVisible();
  expect(page.getByRole('button', { name: '设置', exact: true }).query()).toBeNull();
  await page.getByRole('button', { name: '发起新对话', exact: true }).click();
  await expect.element(page.getByRole('combobox', { name: 'Message', exact: true })).toBeVisible();
});

it('removes the token before probing an existing share cookie', async () => {
  await page.viewport(1440, 1000);
  const probeHashes: string[] = [];
  const { requests, storage } = mountShare('track', true, {
    hash: '#cookie-secret', onProbe: () => { probeHashes.push(window.location.hash); },
  });
  expect(window.location.hash).toBe('');
  await expect.element(page.getByRole('button', { name: /Conversation Planner/ })).toBeVisible();
  expect(probeHashes.length).toBeGreaterThan(0);
  expect(probeHashes.every(hash => hash === '')).toBe(true);
  expect(requests.some(request => request.path.endsWith('/unlock'))).toBe(false);
  expect(Array.from({ length: storage.length }, (_, i) => storage.getItem(storage.key(i)!)).join('')).not.toContain('cookie-secret');
});
it('removes the token synchronously while whoami is pending and after it fails', async () => {
  let fail!: (error: Error) => void;
  const pending = new Promise<Response>((_resolve, reject) => { fail = reject; });
  const probeHashes: string[] = [];
  const { requests } = mountShare('track', false, {
    hash: '#pending-secret', whoami: () => pending, onProbe: () => { probeHashes.push(window.location.hash); },
  });
  expect(window.location.hash).toBe('');
  expect(window.history.state).toEqual({ preserved: true });
  expect(window.location.pathname).toBe('/next/share/link/track/w1');
  await waitFor(() => expect(probeHashes.length).toBeGreaterThan(0));
  expect(probeHashes.every(hash => hash === '')).toBe(true);
  fail(new TypeError('network unavailable'));
  await expect.element(page.getByText('Could not check your session.')).toBeVisible();
  expect(window.location.hash).toBe('');
  expect(requests.some(request => request.path.endsWith('/unlock'))).toBe(false);
});
it('passes the captured token through the production StrictMode mount for one unlock', async () => {
  await page.viewport(1440, 1000);
  const { requests, storage } = mountShare('track', false, { hash: '#secret%2Fonce%2B%23%3F' });
  expect(window.location.hash).toBe('');
  await expect.element(page.getByRole('button', { name: /Conversation Planner/ })).toBeVisible();
  expect(requests.filter(request => request.path.endsWith('/unlock')).map(request => request.body))
    .toEqual([{ token: 'secret/once+#?' }]);
  expect(window.location.hash).toBe('');
  expect(Array.from({ length: storage.length }, (_, i) => storage.getItem(storage.key(i)!)).join('')).not.toContain('secret');
});
