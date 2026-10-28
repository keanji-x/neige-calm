import { describe, expect, it } from 'vitest';

import { decodeWireEvent } from './schemas.js';

describe('core/api wire decode behavior', () => {
  it('decodes registered Assistant input requests with the Rust wire fields and u32 version', () => {
    const data = {
      card_id: 'card-01', track_id: 'track-01', original_worker: 'worker-01',
      source: 'today-summary', subject: 'summary-01', version: 1, text: 'Frozen summary input.',
    };
    for (const version of [0, 1, 4294967295]) {
      const payload = { ev: 'assistant.input_requested', data: { ...data, version } };
      expect(decodeWireEvent(payload)).toEqual({ status: 'ready', value: payload });
    }
    // These are String fields on Event; producer acceptance rules belong to the Rust owner.
    expect(decodeWireEvent({ ev: 'assistant.input_requested', data: {
      ...data, subject: '', text: '',
    } }).status).toBe('ready');
  });

  it('rejects missing or mistyped Assistant input fields, unregistered sources and non-u32 versions', () => {
    const data = {
      card_id: 'card-01', track_id: 'track-01', original_worker: 'worker-01',
      source: 'today-summary', subject: 'summary-01', version: 1, text: 'Frozen summary input.',
    };
    const reject = (value: unknown) => {
      const decoded = decodeWireEvent({ ev: 'assistant.input_requested', data: value });
      expect(decoded.status).toBe('failed');
      if (decoded.status === 'failed') expect(decoded.error.kind).toBe('decode');
    };
    for (const key of Object.keys(data)) {
      const missing: Record<string, unknown> = { ...data };
      delete missing[key];
      reject(missing);
      reject({ ...data, [key]: null });
      reject({ ...data, [key]: key === 'version' ? '1' : 1 });
    }
    for (const source of ['TodaySummary', 'today_summary', 'unregistered']) reject({ ...data, source });
    for (const version of [-1, 0.5, 4294967296, Infinity, NaN]) reject({ ...data, version });
  });

  it('forge_pr_checks preserves diagnostics and independent collection completeness', () => {
    const data = {
      track_id: 'track-01', pr_number: 1, conclusion: 'failure',
      snapshot: { head_sha: 'exact-head', mergeable: 'mergeable', all_checks_completed: false },
      failed_checks: [
        { name: 'shard-1', id: 'C1', diagnostics: {
          status: 'available', failed_tests: ['case_one'], failed_steps: [], error_summary: 'assertion failed',
          log_url: 'https://github.com/o/r/actions/runs/1/job/2', truncated: false,
        } },
        { name: 'shard-2', id: 'C2', diagnostics: { status: 'unavailable', reason: 'no log permission' } },
      ],
    };
    const decoded = decodeWireEvent({ ev: 'forge.pr.checks', data });
    expect(decoded.status).toBe('ready');
    if (decoded.status === 'ready') expect(decoded.value).toEqual({ ev: 'forge.pr.checks', data });
    for (const diagnostics of [{ status: 'available' }, { status: 'unavailable' }, { status: 'unknown' }]) {
      expect(decodeWireEvent({ ev: 'forge.pr.checks', data: {
        ...data, failed_checks: [{ name: 'shard', id: 'C1', diagnostics }],
      } }).status).toBe('failed');
    }
  });
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
  it('accepts failed checks with a URL or forge id but rejects one without a locator', () => {
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


describe('retired ratify events', () => {
  it('rejects requests and both historical decisions', () => {
    for (const frame of [
      { ev: 'ratify.requested', data: { track_id: 'track', reason: 'Merge?' } },
      { ev: 'ratify.resolved', data: { track_id: 'track', decision: 'grant' } },
      { ev: 'ratify.resolved', data: { track_id: 'track', decision: 'deny', message: 'Hold' } },
    ]) {
      expect(decodeWireEvent(frame).status).toBe('failed');
    }
  });
});
