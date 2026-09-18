import { afterEach, expect, it, vi } from 'vitest';
import type { SessionIdentity } from '../../../../core/api/auth.ts';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { logoutMarkerKey } from '../../../../core/domain/recovery/context.ts';
import { RecoverySession } from '../../systems/recovery/session.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { loginForRecovery } from './login.ts';

afterEach(() => vi.unstubAllGlobals());
function setup() {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const values = new Map<string, string>([[logoutMarkerKey(), JSON.stringify({ schemaVersion: 1, fingerprint: 'a'.repeat(64) })]]);
  const access = new RecoveryAccess();
  const proofs: { signal: AbortSignal; finish(identity: SessionIdentity): void }[] = [];
  const posts: { retiredProof: boolean; generation: number; finish(response: ApiTransportResponse): void }[] = [];
  const session = new RecoverySession({ access, origin: 'https://server.test', compatibleVersion: 29,
    storage: { getItem: key => values.get(key) ?? null, setItem: (key, value) => { values.set(key, value); }, removeItem: key => { values.delete(key); } },
    identity: signal => new Promise(resolve => { proofs.push({ signal, finish: resolve }); }),
    version: () => Promise.resolve({ webCompatVersion: 29, minWebCompatVersion: 29, syncEventVersion: 20, dbInstanceId: 'db' }),
    logout: () => Promise.resolve(), clear() {}, adoptScope() {}, online: () => true, visible: () => true });
  const transport = createRecoveryTransports({ send: () => new Promise(resolve => {
    posts.push({ retiredProof: proofs[0]?.signal.aborted ?? false, generation: access.read().generation, finish: resolve });
  }) }, access).probe;
  session.start();
  const identity = (sessionId: string): SessionIdentity => ({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId });
  const finishPost = (sessionId: string) => posts[0].finish({ status: 200, statusText: 'OK', body: identity(sessionId) });
  return { session, access, proofs, posts, transport, identity, finishPost, values };
}

it('manual admission retires a pending pairing proof before POST and retains that generation through verification', async () => {
  const h = setup();
  try {
    const oldPair = h.session.verifyNewSession();
    const manual = loginForRecovery(h.transport, h.session, 'owner', 'password', new AbortController().signal)
      .catch((error: unknown) => error);
    expect(h.posts).toHaveLength(1);
    const retiredBeforePost = h.posts[0].retiredProof;
    h.finishPost('manual');
    await vi.waitFor(() => expect(h.proofs).toHaveLength(2));
    const proofGeneration = h.access.read().generation;
    h.proofs[1].finish(h.identity('manual'));
    const result = await manual;
    h.proofs[0].finish(h.identity('old-pair'));
    expect(await oldPair).toBeNull();
    expect(retiredBeforePost).toBe(true);
    expect(proofGeneration).toBe(h.posts[0].generation);
    expect(result).toEqual(h.identity('manual'));
    expect(h.session.identity?.sessionId).toBe('manual');
    expect(h.values.has(logoutMarkerKey())).toBe(false);
  } finally { h.session.stop(); }
});

it('retired manual cancellation and completion cannot revoke a newer pairing proof', async () => {
  const h = setup(); const form = new AbortController();
  try {
    const manual = loginForRecovery(h.transport, h.session, 'owner', 'password', form.signal).catch((error: unknown) => error);
    const newPair = h.session.verifyNewSession();
    form.abort(); h.finishPost('old-manual');
    await manual;
    const keptNewProof = !h.proofs[0].signal.aborted;
    h.proofs[0].finish(h.identity('new-pair'));
    const result = await newPair;
    expect(keptNewProof).toBe(true);
    expect(result?.sessionId).toBe('new-pair');
    expect(h.session.identity?.sessionId).toBe('new-pair');
    expect(h.proofs).toHaveLength(1);
    expect(h.values.has(logoutMarkerKey())).toBe(false);
  } finally { h.session.stop(); }
});
