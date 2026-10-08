import { afterEach, expect, it, vi } from 'vitest';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { createRecoveryTransports } from './transport.ts';
import type { ApiAbortSignal, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { SessionIdentity } from '../../../../core/api/auth.ts';
import { RecoverySession, type RecoverySessionPorts } from './session.ts';
import { observeRecoveryLifecycle } from './public.tsx';

const releases: Array<() => void> = [];
afterEach(() => { releases.splice(0).reverse().forEach(release => release()); vi.restoreAllMocks(); });
function harness() {
  const access = new RecoveryAccess();
  const pending: Array<{ signal: AbortSignal; finish(identity: SessionIdentity): void }> = [];
  const identity = vi.fn((signal: AbortSignal) => new Promise<SessionIdentity>(finish => { pending.push({ signal, finish }); }));
  const version = vi.fn(() => Promise.resolve({ webCompatVersion: 46, minWebCompatVersion: 46, syncEventVersion: 27, dbInstanceId: 'db' }));
  const values = new Map<string, string>();
  const ports: RecoverySessionPorts = { access, identity, version, origin: 'http://localhost:4140', compatibleVersion: 46,
    storage: { getItem: key => values.get(key) ?? null, setItem: (key, value) => { values.set(key, value); }, removeItem: key => { values.delete(key); } },
    logout: () => Promise.resolve(), clear: () => undefined, adoptScope: () => undefined, online: () => true, visible: () => true };
  const session = new RecoverySession(ports); releases.push(() => session.stop());
  return { access, pending, identity, version, session };
}
it('joins adjacent lifecycle wake notifications without cancelling the current identity proof', () => {
  const h = harness(); h.session.start();
  vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
  releases.push(observeRecoveryLifecycle(h.session));
  const generation = h.access.read().generation;
  window.dispatchEvent(new Event('pageshow'));
  window.dispatchEvent(new Event('online'));
  document.dispatchEvent(new Event('visibilitychange'));
  expect(h.identity).toHaveBeenCalledTimes(1);
  expect(h.pending[0].signal.aborted).toBe(false);
  expect(h.access.read().generation).toBe(generation);
  expect(() => h.access.capture()).toThrow();
});
it('keeps forced authorization revalidation able to cancel an in-flight proof', () => {
  const h = harness(); h.session.start();
  const generation = h.access.read().generation;
  h.session.resume();
  expect(h.pending[0].signal.aborted).toBe(true);
  expect(h.identity).toHaveBeenCalledTimes(2);
  expect(h.access.read().generation).toBeGreaterThan(generation);
});
it('pauses on pagehide even when the browser has not changed document.hidden', () => {
  const h = harness(); h.session.start();
  vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
  releases.push(observeRecoveryLifecycle(h.session));
  window.dispatchEvent(new Event('pagehide'));
  expect(h.pending[0].signal.aborted).toBe(true);
  expect(h.identity).toHaveBeenCalledTimes(1);
  expect(h.access.read().phase).toBe('paused');
});

const acceptedIdentity: SessionIdentity = Object.freeze({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'current-session' });
async function settle() { for (let index = 0; index < 12; index++) await Promise.resolve(); }
it('retains an active page read and permit on a duplicate wake, but revokes both on a real pause', async () => {
  const h = harness(); h.session.start(); h.pending[0].finish(acceptedIdentity); await settle();
  h.session.events('connected');
  const permit = h.access.capture();
  let requestSignal!: ApiAbortSignal; let finish!: (response: ApiTransportResponse) => void;
  const base: ApiTransportPort = { send: request => { requestSignal = request.signal!; return new Promise(resolve => { finish = resolve; }); } };
  const read = createRecoveryTransports(base, h.access).business.send({ method: 'GET', path: '/api/tracks/track', credentials: 'include' });
  const verdict = read.then(() => 'accepted', () => 'rejected');
  h.session.wake(); h.session.wake();
  expect(h.pending).toHaveLength(1); expect(requestSignal.aborted).toBe(false);
  expect(() => h.access.check(permit)).not.toThrow();
  h.session.pause();
  expect(requestSignal.aborted).toBe(true); expect(() => h.access.check(permit)).toThrow();
  finish({ status: 200, statusText: 'OK', body: { old: true } });
  expect(await verdict).toBe('rejected');
});
it('ignores a retired identity result after a real pause and starts fresh verification on wake', async () => {
  const h = harness(); h.session.start();
  h.session.pause(); h.session.wake();
  expect(h.pending).toHaveLength(2); expect(h.pending[0].signal.aborted).toBe(true);
  h.pending[0].finish(acceptedIdentity); await settle();
  expect(h.version).not.toHaveBeenCalled(); expect(h.session.identity).toBeNull();
  h.pending[1].finish(acceptedIdentity); await settle();
  expect(h.version).toHaveBeenCalledOnce(); expect(h.access.read().phase).toBe('syncing');
  expect(() => h.access.capture()).toThrow();
});
