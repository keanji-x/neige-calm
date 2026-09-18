import { describe, expect, it } from 'vitest';
import { decodeScanClaim, decodeScanContext, decodeScanRedeem } from './scan.js';
function context() { return { generation: 7, origin: 'https://neige.tail.example', enrollmentId: 'invite', attemptId: 'attempt', attemptSecret: 'a'.repeat(64), pairTicket: 'b'.repeat(64), deadline: 100_000 }; }
describe('single-document scan contract', () => {
  it('accepts the bounded native projection and freezes it', () => {
    const value = decodeScanContext(context(), context().origin, 1_000);
    expect(value).toEqual(context()); expect(Object.isFrozen(value)).toBe(true);
  });
  it('rejects another origin, auth key disclosure, expiry and malformed attempts', () => {
    for (const mutation of [{ origin: 'https://other.example' }, { authKey: 'tskey-auth-secret' }, { generation: 0 }, { generation: 2 ** 54 }, { deadline: 1_000 }, { deadline: 300_000 }, { attemptSecret: 'secret' }, { pairTicket: 'A'.repeat(64) }, { enrollmentId: '../other' }]) {
      expect(decodeScanContext({ ...context(), ...mutation }, context().origin, 1_000)).toBeNull();
    }
  });
  it('binds both responses to exactly this enrollment and attempt', () => {
    const scan = context();
    expect(decodeScanClaim({ enrollmentId: 'invite', attemptId: 'attempt', claimId: 'claim' }, scan).claimId).toBe('claim');
    expect(decodeScanRedeem({ enrollmentId: 'invite', attemptId: 'attempt', sessionFingerprint: 'c'.repeat(64) }, scan).sessionFingerprint).toBe('c'.repeat(64));
    for (const mutation of [{ enrollmentId: 'other' }, { attemptId: 'other' }, { sessionId: 'raw-session' }]) {
      expect(() => decodeScanRedeem({ enrollmentId: 'invite', attemptId: 'attempt', sessionFingerprint: 'c'.repeat(64), ...mutation }, scan)).toThrow();
    }
  });
});
