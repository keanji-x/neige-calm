import { describe, expect, it } from 'vitest';

import { decodeWireEvent } from './schemas.js';

describe('core/api wire decode behavior', () => {
  it('accepts historical checks and complete snapshots but rejects incomplete evidence', () => {
    const data = { track_id: 'track-01', pr_number: 1, conclusion: 'success' };
    expect(decodeWireEvent({ ev: 'forge.pr.checks', data }).status).toBe('ready');
    expect(decodeWireEvent({ ev: 'forge.pr.checks', data: {
      ...data, snapshot: { head_sha: 'exact-head', mergeable: 'mergeable' },
    } }).status).toBe('ready');
    for (const snapshot of [{ head_sha: 'exact-head' }, { mergeable: 'mergeable' }]) {
      expect(decodeWireEvent({ ev: 'forge.pr.checks', data: { ...data, snapshot } }).status).toBe('failed');
    }
  });
  it('accepts failed checks with a URL or node id but rejects one without a locator', () => {
    const data = {
      track_id: 'track-01', pr_number: 1, conclusion: 'failure',
      snapshot: { head_sha: 'exact-head', mergeable: 'mergeable' },
    };
    const failed_checks = [{ name: 'lint', url: 'https://ci.example/lint' }, { name: 'legacy status', id: 'SC_kw1' }];
    const decoded = decodeWireEvent({ ev: 'forge.pr.checks', data: { ...data, failed_checks } });
    expect(decoded.status).toBe('ready');
    if (decoded.status === 'ready') expect(decoded.value).toEqual({ ev: 'forge.pr.checks', data: { ...data, failed_checks } });
    expect(decodeWireEvent({ ev: 'forge.pr.checks', data: { ...data, failed_checks: [{ name: 'lint' }] } }).status)
      .toBe('failed');
  });
  it('returns unknown frames as decode data so callers can log and skip', () => {
    const result = decodeWireEvent({ ev: 'future.event', data: { version: 2 } });
    expect(result.status).toBe('failed');
    if (result.status === 'failed') expect(result.error.kind).toBe('decode');
  });
});


describe('synchronous publication receipts', () => {
  it('decodes the receipt while requiring all routing and PR fields', () => {
    const data = { track_id: 'track', pr_number: 2169, head_sha: 'head' };
    expect(decodeWireEvent({ ev: 'forge.pr.published', data }).status).toBe('ready');
    expect(decodeWireEvent({ ev: 'forge.pr.published', data: { track_id: 'track' } }).status).toBe('failed');
  });
});
