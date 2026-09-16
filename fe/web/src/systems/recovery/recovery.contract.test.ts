import { afterEach, expect, it, vi } from 'vitest';
import { z } from 'zod';
import type { SessionIdentity } from '../../../../core/api/auth.ts';
import { performApiRequest } from '../../../../core/api/client.ts';
import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { logoutMarkerKey } from '../../../../core/domain/recovery/context.ts';
import { createRecoveryTransports } from './transport.ts';
import { createRecoveryUnauthorizedChannel } from './unauthorized.ts';
import { RecoverySession } from './session.ts';

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });
const identity: SessionIdentity = Object.freeze({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'secret-session' });
const version = Object.freeze({ webCompatVersion: 28, minWebCompatVersion: 28, syncEventVersion: 3, dbInstanceId: 'db-a' });
function harness() {
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); } };
  const access = new RecoveryAccess();
  const ports = { access, storage, origin: 'http://192.168.1.8:4140', compatibleVersion: 28, adoptScope: vi.fn(),
    identity: vi.fn(() => Promise.resolve(identity)), version: vi.fn(() => Promise.resolve(version)), logout: vi.fn(() => Promise.resolve(undefined)),
    clear: vi.fn(), online: vi.fn(() => true), visible: () => true };
  return { values, ports, session: new RecoverySession(ports), access };
}
it('late business success and 401 cannot escape a changed recovery generation', async () => {
  for (const status of [200, 401]) {
    const access = new RecoveryAccess(); access.change('connected');
    let finish!: (reply: ApiTransportResponse) => void;
    const base: ApiTransportPort = { send: () => new Promise(resolve => { finish = resolve; }) };
    const { business } = createRecoveryTransports(base, access);
    const channel = createRecoveryUnauthorizedChannel(access, { enqueue: task => task() });
    const denied = vi.fn(); channel.subscribe(denied);
    const request = performApiRequest(business, { method: 'GET', path: '/api/private', responseSchema: z.unknown() }, channel);
    access.invalidate('recovering'); access.change('connected');
    finish({ status, statusText: '', body: 'old private content' });
    expect((await request).status).toBe('failed'); expect(denied).not.toHaveBeenCalled();
  }
});
it('a queued unauthorized delivery cannot reach a newly mounted identity owner', () => {
  const access = new RecoveryAccess(); const queued: (() => void)[] = [];
  const channel = createRecoveryUnauthorizedChannel(access, { enqueue: task => { queued.push(task); } });
  const old = vi.fn(); const remove = channel.subscribe(old); channel.notify();
  remove(); access.invalidate('recovering'); const fresh = vi.fn(); channel.subscribe(fresh);
  queued.shift()!(); expect(fresh).not.toHaveBeenCalled(); expect(old).not.toHaveBeenCalled();
  channel.notify(); queued.shift()!(); expect(fresh).toHaveBeenCalledOnce();
});
it('an immutable admitted transport cannot send after recovery even when connected again', async () => {
  const access = new RecoveryAccess(); access.change('connected'); const send = vi.fn();
  const business = createRecoveryTransports({ send }, access).business;
  const admitted = business.recovery!.scope(business.recovery!.capture());
  access.invalidate('recovering'); access.change('connected');
  await expect(admitted.send({ method: 'POST', path: '/api/write', credentials: 'include' })).rejects.toThrow();
  expect(send).not.toHaveBeenCalled();
});
it('identity precedes version, repeated retries join, and replay completion alone grants writes', async () => {
  const h = harness(); let identityReady!: (value: typeof identity) => void;
  h.ports.identity.mockImplementation(() => new Promise(resolve => { identityReady = resolve; }));
  h.session.start(); h.session.retry(); h.session.retry();
  expect(h.ports.identity).toHaveBeenCalledOnce(); expect(h.ports.version).not.toHaveBeenCalled();
  identityReady(identity); await vi.waitFor(() => expect(h.access.read().phase).toBe('syncing'));
  expect(() => h.access.capture()).toThrow(); h.session.events('connected'); expect(() => h.access.capture()).not.toThrow();
  h.session.pause(); expect(h.session.identity).toEqual(identity); expect(() => h.access.capture()).toThrow(); h.session.stop();
});
it('offline logout stores only a SHA-256 denial and requires an explicit different session after restart', async () => {
  const h = harness(); h.session.start(); await vi.waitFor(() => expect(h.access.read().phase).toBe('syncing'));
  h.ports.online.mockReturnValue(false); await h.session.signOut(); h.session.stop();
  expect(h.ports.logout).not.toHaveBeenCalled(); const marker = h.values.get(logoutMarkerKey())!;
  expect(marker).not.toContain(identity.sessionId); expect((JSON.parse(marker) as { fingerprint: string }).fingerprint).toMatch(/^[a-f0-9]{64}$/);
  h.ports.online.mockReturnValue(true); h.ports.identity.mockClear(); const resumed = new RecoverySession(h.ports);
  resumed.start(); expect(h.ports.identity).not.toHaveBeenCalled();
  expect(await resumed.verifyNewSession()).toBeNull(); expect(h.values.has(logoutMarkerKey())).toBe(true);
  h.ports.identity.mockResolvedValue({ ...identity, sessionId: 'new-secret' });
  expect(await resumed.verifyNewSession()).not.toBeNull(); expect(h.values.has(logoutMarkerKey())).toBe(false); resumed.stop();
});
it('storage failure keeps this document logged out and reports that durable logout failed', async () => {
  const h = harness(); h.session.start(); await vi.waitFor(() => expect(h.access.read().phase).toBe('syncing'));
  h.ports.storage.setItem = () => { throw new Error('full'); }; h.ports.online.mockReturnValue(false);
  await h.session.signOut(); expect(h.access.read().phase).toBe('login');
  expect(h.access.read().detail).toContain('不能保证重开后仍退出'); expect(h.session.identity).toBeNull(); h.session.stop();
});
it('an eight second hung probe times out and its late identity cannot authorize a new generation', async () => {
  vi.useFakeTimers(); const h = harness(); let finish!: (value: typeof identity) => void;
  h.ports.identity.mockImplementation(() => new Promise(resolve => { finish = resolve; })); h.session.start();
  await vi.advanceTimersByTimeAsync(8000); expect(h.access.read().phase).toBe('offline');
  h.session.pause(); finish(identity); await Promise.resolve(); expect(h.ports.version).not.toHaveBeenCalled(); h.session.stop();
});

it('a response invalidated after transport completion is rejected before its 401 broadcasts', async () => {
  for (const status of [200, 401]) {
    const access = new RecoveryAccess(); access.change('connected');
    const business = createRecoveryTransports({ send: () => Promise.resolve({ status, statusText: '', body: 'old' }) }, access).business;
    const transport: ApiTransportPort = { recovery: business.recovery, send: async request => {
      const response = await business.send(request);
      queueMicrotask(() => access.invalidate('recovering'));
      return response;
    } };
    const channel = createRecoveryUnauthorizedChannel(access, { enqueue: task => task() }); const listener = vi.fn(); channel.subscribe(listener);
    const result = await performApiRequest(transport, { method: 'GET', path: '/api/private', responseSchema: z.unknown() }, channel);
    expect(result.status).toBe('failed'); expect(listener).not.toHaveBeenCalled();
  }
});
