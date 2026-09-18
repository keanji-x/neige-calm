import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { logoutMarkerKey } from '../../../../core/domain/recovery/context.ts';
import { mountProductionApp } from './production-app.tsx';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { createIsolatedRetryFixture } from '../router/isolated-task-retry-fixture.tsx';
import type { ApiRequest } from '../../../../core/api/types.ts';
import type { SessionIdentity } from '../../../../core/api/auth.ts';

const mounted = vi.hoisted(() => ({ roots: [] as { unmount(): void }[], logins: [] as Promise<SessionIdentity | null>[] }));
vi.mock('./login.ts', async importOriginal => {
  const actual = await importOriginal<typeof import('./login.ts')>();
  return { ...actual, loginForRecovery: (...args: Parameters<typeof actual.loginForRecovery>) => {
    const result = actual.loginForRecovery(...args); mounted.logins.push(result); return result;
  } };
});
vi.mock('react-dom/client', async importOriginal => {
  const actual = await importOriginal<typeof import('react-dom/client')>();
  return { ...actual, createRoot: (...args: Parameters<typeof actual.createRoot>) => {
    const root = actual.createRoot(...args); mounted.roots.push(root); return root;
  } };
});

it.each([
  { stage: 'login', cancel: true }, { stage: 'identity', cancel: true },
  { stage: 'version', cancel: true }, { stage: 'version', cancel: false },
])('production manual login during $stage respects cancellation=$cancel', async ({ stage, cancel }) => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const marker = JSON.stringify({ schemaVersion: 1, fingerprint: 'a'.repeat(64) });
  const values = new Map<string, string>([[logoutMarkerKey(), marker]]);
  const storage: Storage = { get length() { return values.size; }, getItem: key => values.get(key) ?? null,
    setItem: (key, value) => { values.set(key, value); }, removeItem: key => { values.delete(key); },
    clear: () => values.clear(), key: index => [...values.keys()][index] ?? null };
  const identity = { userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'fresh-session' };
  const version = { webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
    syncEventVersion: 20, dbInstanceId: 'db' };
  let finish!: () => void;
  const pending = new Promise<void>(resolve => { finish = resolve; });
  const requests: string[] = [];
  const fetch = vi.fn(async (input: RequestInfo | URL) => {
    const path = input instanceof Request ? input.url : input.toString(); requests.push(path);
    if ((stage === 'login' && path.endsWith('/login')) || (stage === 'identity' && path.endsWith('/whoami'))
      || (stage === 'version' && path.endsWith('/version'))) await pending;
    return new Response(JSON.stringify(path.endsWith('/version') ? version
      : path.endsWith('/whoami') || path.endsWith('/login') ? identity : []), { status: 200 });
  });
  vi.stubGlobal('fetch', fetch);
  vi.stubGlobal('WebSocket', class {
    onopen = null; onclose = null; onmessage = null; onerror = null;
    send() {} close() {}
  });
  const root = document.createElement('div'); document.body.append(root);
  act(() => mountProductionApp(root, { storage, reload: vi.fn(), deleteDatabase: vi.fn() }));
  fireEvent.click(await screen.findByRole('button', { name: '使用账号登录' }));
  fireEvent.change(screen.getByLabelText('Username'), { target: { value: 'owner' } });
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'password' } });
  fireEvent.submit(screen.getByLabelText('Password').closest('form')!);
  const path = stage === 'identity' ? '/whoami' : `/${stage}`;
  try {
    await waitFor(() => expect(requests.some(request => request.endsWith(path))).toBe(true));
    if (cancel) fireEvent.click(screen.getByRole('button', { name: '返回扫码连接' }));
    expect(mounted.logins).toHaveLength(1);
    await act(async () => { finish(); await Promise.allSettled(mounted.logins); });
    if (cancel) {
      expect(values.get(logoutMarkerKey())).toBe(marker);
      expect(screen.getByRole('button', { name: '使用账号登录' })).toBeTruthy();
      if (stage === 'login') expect(requests.some(request => request.endsWith('/whoami'))).toBe(false);
      expect(requests.filter(request => request.endsWith('/login'))).toHaveLength(1);
      fireEvent.click(screen.getByRole('button', { name: '使用账号登录' }));
      fireEvent.submit(screen.getByLabelText('Password').closest('form')!);
      await waitFor(() => expect(values.has(logoutMarkerKey())).toBe(false));
      expect(requests.filter(request => request.endsWith('/login'))).toHaveLength(2);
    } else {
      await waitFor(() => expect(values.has(logoutMarkerKey())).toBe(false));
      expect(screen.queryByRole('button', { name: '使用账号登录' })).toBeNull();
    }
    if (!cancel) expect(requests.filter(request => request.endsWith('/login'))).toHaveLength(1);
  } finally { finish(); }
});

it.each([false, true])('production event owners retire before compatible protocol changes (new database=%s)', async newDatabase => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  window.history.replaceState({}, '', '/next/track/w1');
  const fixture = createIsolatedRetryFixture();
  const values = new Map<string, string>();
  const storage: Storage = { get length() { return values.size; }, getItem: key => values.get(key) ?? null,
    setItem: (key, value) => { values.set(key, value); }, removeItem: key => { values.delete(key); },
    clear: () => values.clear(), key: index => [...values.keys()][index] ?? null };
  let version = { webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
    syncEventVersion: 19, dbInstanceId: 'first-db' };
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = input instanceof Request ? input.url : input.toString();
    if (path.endsWith('/whoami')) return new Response(JSON.stringify({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 's' }));
    if (path.endsWith('/version')) return new Response(JSON.stringify(version));
    const response = await fixture.transport.send({ path, method: (init?.method ?? 'GET') as ApiRequest['method'], credentials: 'include' });
    return new Response(JSON.stringify(response.body), { status: response.status });
  }));
  const sockets: Socket[] = []; let peak = 0;
  class Socket {
    static OPEN = 1; readyState = 0; closed = false;
    onopen: ((event: Event) => void) | null = null;
    onclose: ((event: CloseEvent) => void) | null = null;
    onmessage: ((event: MessageEvent) => void) | null = null;
    onerror: ((event: Event) => void) | null = null;
    constructor() { sockets.push(this); peak = Math.max(peak, sockets.filter(socket => !socket.closed).length); }
    send() {}
    close() { this.closed = true; this.readyState = 3; }
    replay() { this.readyState = 1; this.onopen?.(new Event('open')); this.onmessage?.(new MessageEvent('message', { data: '{"ev":"_replay_complete","_id":0}' })); }
  }
  vi.stubGlobal('WebSocket', Socket);
  const root = document.createElement('div'); document.body.append(root);
  act(() => mountProductionApp(root, { storage, reload: vi.fn(), deleteDatabase: vi.fn() }));
  await waitFor(() => expect(sockets.filter(socket => !socket.closed)).toHaveLength(1));
  const original = sockets.at(-1)!;
  act(() => original.replay());
  await screen.findByText(fixture.goal); await screen.findByText('已连接');
  const stale = original.onmessage;
  act(() => {
    version = { ...version, syncEventVersion: 20, dbInstanceId: newDatabase ? 'second-db' : 'first-db' };
    window.dispatchEvent(new Event('online'));
  });
  await waitFor(() => expect(original.closed).toBe(true));
  await waitFor(() => expect(sockets.at(-1)).not.toBe(original));
  expect(sockets.filter(socket => !socket.closed)).toHaveLength(1); expect(peak).toBe(1);
  act(() => stale?.(new MessageEvent('message', { data: '{"ev":"_replay_complete","_id":0}' })));
  expect(screen.queryByText('已连接')).toBeNull();
  act(() => sockets.at(-1)!.replay());
  await screen.findByText('已连接'); await screen.findByText(fixture.goal);
  expect(window.location.pathname).toBe('/next/track/w1');
  expect(fixture.requests.filter(request => request.method !== 'GET')).toEqual([]);
});
afterEach(() => { act(() => { for (const root of mounted.roots.splice(0)) root.unmount(); }); mounted.logins.length = 0; document.body.replaceChildren(); vi.unstubAllGlobals(); });
it('a logged-out cold production mount exposes explicit pairing verification inside the phone viewport', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true); await page.viewport(390, 844);
  const fetch = vi.fn(); vi.stubGlobal('fetch', fetch);
  const values = new Map<string, string>([[logoutMarkerKey(), JSON.stringify({ schemaVersion: 1, fingerprint: 'a'.repeat(64) })]]);
  const storage: Storage = { get length() { return values.size; }, getItem: key => values.get(key) ?? null,
    setItem: (key, value) => { values.set(key, value); }, removeItem: key => { values.delete(key); },
    clear: () => values.clear(), key: index => [...values.keys()][index] ?? null };
  const root = document.createElement('div'); document.body.append(root);
  act(() => mountProductionApp(root, { storage, reload: vi.fn(), deleteDatabase: vi.fn() }));
  const verify = await screen.findByRole('button', { name: '验证本次配对' });
  expect(fetch).not.toHaveBeenCalled();
  const bounds = verify.getBoundingClientRect();
  expect(bounds.top).toBeGreaterThanOrEqual(0); expect(bounds.bottom).toBeLessThanOrEqual(844);
});
