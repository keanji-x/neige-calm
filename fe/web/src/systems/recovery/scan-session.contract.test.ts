import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { logoutMarkerKey } from '../../../../core/domain/recovery/context.ts';
import type { ScanContext } from '../../../../core/domain/recovery/scan.ts';
import { createScanPairingPort } from './scan.ts';
import { RecoverySession } from './session.ts';
import { createRecoveryTransports } from './transport.ts';
const fresh = '6542995698f2176ac198ef55125a6924b40e7cd07653a76c1157da771b2c9bf9';
const old = '13c49aa4e416f7a6e0f164f9aa999c65b7fbfb6d4d06329a2602997f8cf10985';
const owners: RecoverySession[] = [];
beforeEach(() => { vi.useFakeTimers(); vi.setSystemTime(10_000); });
afterEach(() => { for (const owner of owners.splice(0)) owner.stop(); vi.useRealTimers(); });
function harness() {
  const context: ScanContext = { generation: 5, origin: 'https://neige.tail.example', enrollmentId: 'enroll', attemptId: 'attempt', attemptSecret: 'a'.repeat(64), pairTicket: 'b'.repeat(64), deadline: 100_000 };
  const values = new Map<string, string>([[logoutMarkerKey(), JSON.stringify({ schemaVersion: 1, fingerprint: old })]]);
  const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: vi.fn((key: string) => { values.delete(key); }) };
  const access = new RecoveryAccess(); const order: string[] = [];
  const send = vi.fn((request: { path: string }): Promise<ApiTransportResponse> => {
    order.push(request.path);
    return Promise.resolve({ status: 200, statusText: 'OK', body: request.path.endsWith('/claim')
      ? { enrollmentId: 'enroll', attemptId: 'attempt', claimId: 'claim' }
      : { enrollmentId: 'enroll', attemptId: 'attempt', sessionFingerprint: fresh } });
  });
  const ports = { access, storage, origin: context.origin, compatibleVersion: 28, adoptScope: vi.fn(),
    identity: vi.fn(() => { order.push('whoami'); return Promise.resolve({ userId: 'owner', displayName: 'Owner', role: 'owner' as const, sessionId: 'fresh-session' }); }),
    version: vi.fn(() => { order.push('version'); return Promise.resolve({ webCompatVersion: 28, minWebCompatVersion: 28, syncEventVersion: 3, dbInstanceId: 'db' }); }),
    logout: vi.fn(() => Promise.resolve(undefined)), clear: vi.fn(), online: () => true, visible: () => true };
  const session = new RecoverySession(ports, { input: { kind: 'scan', context }, pairing: createScanPairingPort({ send }) }); owners.push(session);
  return { session, ports, values, access, send, order, context };
}
const settle = () => vi.advanceTimersByTimeAsync(0);
describe('production scan session owner', () => {
  it('orders claim redeem actual-cookie proof version and replay before business writes', async () => {
    const h = harness(); h.session.start(); await settle();
    expect(h.order).toEqual(['/api/mobile/enrollments/claim', '/api/mobile/enrollments/redeem', 'whoami', 'version']);
    expect(h.values.has(logoutMarkerKey())).toBe(false); expect(h.access.read().phase).toBe('syncing');
    const business = createRecoveryTransports({ send: h.send }, h.access).business;
    await expect(business.send({ method: 'POST', path: '/api/mobile/enrollments/redeem', credentials: 'include' })).rejects.toThrow();
    h.session.events('connected'); expect(() => h.access.capture()).not.toThrow();
  });
  it('cannot use ordinary business or recovery transport as an enrollment bypass', async () => {
    const h = harness(); const guarded = createRecoveryTransports({ send: h.send }, h.access);
    for (const transport of [guarded.business, guarded.probe]) await expect(transport.send({ method: 'POST', path: '/api/mobile/enrollments/claim', credentials: 'include' })).rejects.toThrow();
    expect(h.send).not.toHaveBeenCalled();
  });
  it('rejects an attempt mismatch before querying cookie identity', async () => {
    const h = harness(); h.send.mockResolvedValue({ status: 200, statusText: 'OK', body: { enrollmentId: 'enroll', attemptId: 'other', claimId: 'claim' } });
    h.session.start(); await settle(); expect(h.ports.identity).not.toHaveBeenCalled();
    expect(h.values.has(logoutMarkerKey())).toBe(true); expect(h.access.read().phase).toBe('login');
  });
  it('rejects a cookie whose fingerprint differs from the redeemed session', async () => {
    const h = harness(); h.ports.identity.mockResolvedValue({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'unrelated-cookie' });
    h.session.start(); await settle(); expect(h.ports.version).not.toHaveBeenCalled();
    expect(h.values.has(logoutMarkerKey())).toBe(true); expect(h.access.read().phase).toBe('login');
  });
  it('rejects the logged-out cookie even if redeem repeats its fingerprint', async () => {
    const h = harness(); h.send.mockImplementation(request => Promise.resolve({ status: 200, statusText: 'OK', body: request.path.endsWith('/claim') ? { enrollmentId: 'enroll', attemptId: 'attempt', claimId: 'claim' } : { enrollmentId: 'enroll', attemptId: 'attempt', sessionFingerprint: old } }));
    h.ports.identity.mockResolvedValue({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'old-session' });
    h.session.start(); await settle(); expect(h.ports.version).not.toHaveBeenCalled(); expect(h.values.has(logoutMarkerKey())).toBe(true);
  });
  it('does not grant authority when marker removal fails', async () => {
    const h = harness(); h.ports.storage.removeItem.mockImplementation(() => { throw new Error('disk'); });
    h.session.start(); await settle(); expect(h.ports.version).not.toHaveBeenCalled(); expect(h.values.has(logoutMarkerKey())).toBe(true); expect(h.session.identity).toBeNull();
  });
  it('cancels a late claim and never redeems after pause', async () => {
    const h = harness(); let finish!: (response: ApiTransportResponse) => void;
    h.send.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    h.session.start(); h.session.pause(); finish({ status: 200, statusText: 'OK', body: { enrollmentId: 'enroll', attemptId: 'attempt', claimId: 'claim' } }); await settle();
    h.session.resume(); await settle(); expect(h.send).toHaveBeenCalledTimes(1); expect(h.ports.identity).not.toHaveBeenCalled(); expect(h.values.has(logoutMarkerKey())).toBe(true);
  });
  it('stale whoami cannot clear the marker or open version after cancellation', async () => {
    const h = harness(); let finish!: (identity: Awaited<ReturnType<typeof h.ports.identity>>) => void;
    h.ports.identity.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    h.session.start(); await settle(); h.session.pause(); finish({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'fresh-session' }); await settle();
    expect(h.ports.version).not.toHaveBeenCalled(); expect(h.values.has(logoutMarkerKey())).toBe(true);
  });
  it('uses ordinary reconnect after accepted identity when version times out', async () => {
    const h = harness(); h.ports.version.mockImplementationOnce(() => new Promise(() => {}));
    h.session.start(); await settle(); expect(h.values.has(logoutMarkerKey())).toBe(false);
    await vi.advanceTimersByTimeAsync(8_001); expect(h.access.read().phase).toBe('offline');
    h.session.retry(); await settle(); expect(h.access.read().phase).toBe('syncing');
    expect(h.send).toHaveBeenCalledTimes(2); expect(h.ports.identity).toHaveBeenCalledTimes(2);
  });
  it('expired context never sends pairing requests and manual verify cannot revive it', async () => {
    const h = harness(); await vi.advanceTimersByTimeAsync(100_000); h.session.start(); await settle();
    expect(h.send).not.toHaveBeenCalled(); await h.session.verifyNewSession(); h.session.resume();
    expect(h.ports.identity).not.toHaveBeenCalled(); expect(h.values.has(logoutMarkerKey())).toBe(true);
  });
});
