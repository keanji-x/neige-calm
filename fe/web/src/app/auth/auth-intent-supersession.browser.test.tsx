import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mountProductionApp } from './production-app.tsx';
import { logoutMarkerKey } from '../../../../core/domain/recovery/context.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { RecoverySession } from '../../systems/recovery/session.ts';
import type { SessionIdentity } from '../../../../core/api/auth.ts';

const mounted = vi.hoisted(() => ({ roots: [] as { unmount(): void }[],
  proofs: [] as Promise<SessionIdentity | null>[], logins: [] as Promise<SessionIdentity | null>[] }));
vi.mock('./login.ts', async importOriginal => {
  const actual = await importOriginal<typeof import('./login.ts')>();
  return { ...actual, loginForRecovery: (...args: Parameters<typeof actual.loginForRecovery>) => {
    const result = actual.loginForRecovery(...args); mounted.logins.push(result); return result;
  } };
});
beforeEach(() => {
  const verify = Object.getOwnPropertyDescriptor(RecoverySession.prototype, 'verifyNewSession')?.value as RecoverySession['verifyNewSession'];
  vi.spyOn(RecoverySession.prototype, 'verifyNewSession').mockImplementation(function (this: RecoverySession, ...args) {
    const result = verify.apply(this, args); mounted.proofs.push(result); return result;
  });
});
vi.mock('react-dom/client', async importOriginal => {
  const actual = await importOriginal<typeof import('react-dom/client')>();
  return { ...actual, createRoot: (...args: Parameters<typeof actual.createRoot>) => {
    const root = actual.createRoot(...args); mounted.roots.push(root); return root;
  } };
});
afterEach(() => {
  act(() => { for (const root of mounted.roots.splice(0)) root.unmount(); });
  mounted.proofs.length = 0; mounted.logins.length = 0;
  document.body.replaceChildren(); vi.unstubAllGlobals(); vi.restoreAllMocks();
});

it('a newer canceled manual login must retire the earlier pairing proof', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const marker = JSON.stringify({ schemaVersion: 1, fingerprint: 'a'.repeat(64) });
  const values = new Map<string, string>([[logoutMarkerKey(), marker]]);
  const storage: Storage = { get length() { return values.size; }, getItem: key => values.get(key) ?? null,
    setItem: (key, value) => { values.set(key, value); }, removeItem: key => { values.delete(key); },
    clear: () => values.clear(), key: index => [...values.keys()][index] ?? null };
  const identity = { userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'paired-session' };
  const version = { webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
    syncEventVersion: 20, dbInstanceId: 'db' };
  let finishPair!: () => void;
  const pairPending = new Promise<void>(resolve => { finishPair = resolve; });
  let finishLogin!: () => void;
  const loginPending = new Promise<void>(resolve => { finishLogin = resolve; });
  const paths: string[] = [];
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
    const path = input instanceof Request ? input.url : input.toString(); paths.push(path);
    if (path.endsWith('/whoami')) await pairPending;
    if (path.endsWith('/login')) await loginPending;
    return new Response(JSON.stringify(path.endsWith('/version') ? version
      : path.endsWith('/whoami') ? identity
        : path.endsWith('/login') ? { ...identity, sessionId: 'manual-session' } : []));
  }));
  vi.stubGlobal('WebSocket', class {
    onopen = null; onclose = null; onmessage = null; onerror = null;
    send() {} close() {}
  });
  const root = document.createElement('div'); document.body.append(root);
  act(() => mountProductionApp(root, { storage, reload: vi.fn(), deleteDatabase: vi.fn() }));
  try {
    fireEvent.click(await screen.findByRole('button', { name: '验证本次配对' }));
    await waitFor(() => expect(paths.some(path => path.endsWith('/whoami'))).toBe(true));
    fireEvent.click(screen.getByRole('button', { name: '使用账号登录' }));
    fireEvent.change(screen.getByLabelText('Username'), { target: { value: 'owner' } });
    fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'password' } });
    fireEvent.submit(screen.getByLabelText('Password').closest('form')!);
    await waitFor(() => expect(paths.some(path => path.endsWith('/login'))).toBe(true));
    fireEvent.click(screen.getByRole('button', { name: '返回扫码连接' }));
    await act(async () => { finishLogin(); finishPair(); await Promise.allSettled([...mounted.proofs, ...mounted.logins]); });
    expect(paths.filter(path => path.endsWith('/whoami'))).toHaveLength(1);
    expect.soft(values.get(logoutMarkerKey())).toBe(marker);
    expect.soft(screen.queryByRole('button', { name: '使用账号登录' })).not.toBeNull();
  } finally { finishLogin(); finishPair(); }
});

function harness() {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const marker = JSON.stringify({ schemaVersion: 1, fingerprint: 'a'.repeat(64) });
  const values = new Map<string, string>([[logoutMarkerKey(), marker]]);
  const storage: Storage = { get length() { return values.size; }, getItem: key => values.get(key) ?? null,
    setItem: (key, value) => { values.set(key, value); }, removeItem: key => { values.delete(key); },
    clear: () => values.clear(), key: index => [...values.keys()][index] ?? null };
  type Held = { signal: AbortSignal | null; finish(sessionId: string): void };
  const proofs: Held[] = []; const posts: Held[] = []; let versions = 0;
  const held = (entries: Held[], signal: AbortSignal | null) => new Promise<Response>(resolve => {
    entries.push({ signal, finish: sessionId => resolve(new Response(JSON.stringify({
      userId: 'owner', displayName: 'Owner', role: 'owner', sessionId,
    }))) });
  });
  vi.stubGlobal('fetch', vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const path = input instanceof Request ? input.url : input.toString();
    if (path.endsWith('/whoami')) return held(proofs, init?.signal ?? null);
    if (path.endsWith('/login')) return held(posts, init?.signal ?? null);
    if (path.endsWith('/version')) {
      versions++;
      return Promise.resolve(new Response(JSON.stringify({ webCompatVersion: WEB_COMPAT_VERSION,
        minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 20, dbInstanceId: 'db' })));
    }
    return Promise.resolve(new Response('[]'));
  }));
  vi.stubGlobal('WebSocket', class {
    onopen = null; onclose = null; onmessage = null; onerror = null;
    send() {} close() {}
  });
  const root = document.createElement('div'); document.body.append(root);
  act(() => mountProductionApp(root, { storage, reload: vi.fn(), deleteDatabase: vi.fn() }));
  return { marker, values, proofs, posts, versions: () => versions,
    pair: async () => {
      const count = proofs.length;
      fireEvent.click(await screen.findByRole('button', { name: '验证本次配对' }));
      await waitFor(() => expect(proofs).toHaveLength(count + 1));
    },
    manual: async () => { fireEvent.click(await screen.findByRole('button', { name: '使用账号登录' })); },
    submit: async () => {
      const count = posts.length;
      fireEvent.submit(screen.getByLabelText('Password').closest('form')!);
      await waitFor(() => expect(posts).toHaveLength(count + 1));
    },
    cancel: () => fireEvent.click(screen.getByRole('button', { name: '返回扫码连接' })),
    settle: () => act(async () => { await Promise.allSettled([...mounted.proofs, ...mounted.logins]); }),
    releaseAll: () => { for (const request of [...proofs, ...posts]) request.finish('retired'); },
  };
}

it('switching to manual entry retires pairing even without submitting credentials', async () => {
  const h = harness();
  try {
    await h.pair(); await h.manual();
    expect(h.proofs[0].signal?.aborted).toBe(true);
    h.proofs[0].finish('old-pair'); await h.settle();
    expect(h.values.get(logoutMarkerKey())).toBe(h.marker);
    expect(screen.getByLabelText('Password')).toBeTruthy();
    expect(h.posts).toHaveLength(0); expect(h.versions()).toBe(0);
  } finally { h.releaseAll(); }
});

it('a successful newer manual login supersedes a held pairing proof', async () => {
  const h = harness();
  try {
    await h.pair(); await h.manual(); await h.submit();
    await act(async () => { h.posts[0].finish('manual'); await Promise.resolve(); });
    await waitFor(() => expect(h.proofs).toHaveLength(2));
    h.proofs[1].finish('manual'); await h.settle();
    expect(h.values.has(logoutMarkerKey())).toBe(false);
    expect(screen.queryByRole('button', { name: '使用账号登录' })).toBeNull();
    h.proofs[0].finish('old-pair'); await h.settle();
    expect(h.versions()).toBe(1); expect(h.posts).toHaveLength(1);
    expect(screen.queryByText('Wrong username or password.')).toBeNull();
  } finally { h.releaseAll(); }
});

it('a newer pairing proof survives the canceled manual POST completing late', async () => {
  const h = harness();
  try {
    await h.manual(); await h.submit(); h.cancel(); await h.pair();
    await act(async () => { h.proofs[0].finish('new-pair'); await mounted.proofs.at(-1); });
    expect(h.values.has(logoutMarkerKey())).toBe(false);
    h.posts[0].finish('old-manual'); await h.settle();
    expect(screen.queryByRole('button', { name: '使用账号登录' })).toBeNull();
    expect(h.proofs).toHaveLength(1); expect(h.versions()).toBe(1);
  } finally { h.releaseAll(); }
});

it('a repeated explicit pairing verification supersedes the older proof', async () => {
  const h = harness();
  try {
    await h.pair(); await h.pair();
    expect(h.proofs[0].signal?.aborted).toBe(true);
    h.proofs[1].finish('new-pair'); await h.settle();
    h.proofs[0].finish('old-pair'); await h.settle();
    expect(h.values.has(logoutMarkerKey())).toBe(false);
    expect(h.versions()).toBe(1); expect(h.posts).toHaveLength(0);
  } finally { h.releaseAll(); }
});
