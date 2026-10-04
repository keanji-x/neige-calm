import { describe, expect, it } from 'vitest';

import {
  failedConversationDelivery, isUnknownSendFailure, retryUnknownSend, SEND_RETRIES,
} from './conversation-delivery.js';

describe('failed conversation delivery', () => {
  it.each([400, 403, 404, 413, 422, 429])('permits an explicit %s rejection to be retried', (status) => {
    expect(failedConversationDelivery({ kind: 'http', status, code: 'rejected', message: 'rejected' })).toBe('rejected');
  });

  it.each([408, 409, 500, 502, 503, 504])('keeps HTTP %s acceptance uncertain', (status) => {
    expect(failedConversationDelivery({ kind: 'http', status, code: 'unavailable', message: 'unavailable' })).toBe('unknown');
  });

  it('keeps transport and decode failures uncertain', () => {
    expect(failedConversationDelivery({ kind: 'transport', message: 'dropped' })).toBe('unknown');
    expect(failedConversationDelivery({ kind: 'decode', message: 'malformed' })).toBe('unknown');
    expect(failedConversationDelivery(null)).toBe('unknown');
  });
});

describe('an unknown send outcome', () => {
  it('is what a lost answer, a server error or no answer at all leaves', () => {
    expect(isUnknownSendFailure({ kind: 'transport', message: 'dropped' })).toBe(true);
    expect(isUnknownSendFailure({ kind: 'http', status: 503, code: 'service_unavailable', message: 'full' })).toBe(true);
    expect(isUnknownSendFailure(null)).toBe(true);
  });

  it('is not a refusal or a rejection, which the server answered', () => {
    expect(isUnknownSendFailure({ kind: 'http', status: 409, code: 'planner_harness_dormant', message: 'reset' })).toBe(false);
    expect(isUnknownSendFailure({ kind: 'http', status: 400, code: 'bad_request', message: 'empty' })).toBe(false);
    expect(isUnknownSendFailure({ kind: 'unauthorized', status: 401, code: 'session_expired', message: 'expired' })).toBe(false);
  });
});

describe('retrying an unknown send', () => {
  const unknown = new Error('unknown');
  const answered = new Error('answered');
  const isUnknown = (error: unknown) => error === unknown;

  it('sends again until an answer arrives', async () => {
    const attempts: number[] = [];
    const pauses: number[] = [];
    const result = await retryUnknownSend((index) => {
      attempts.push(index);
      return index < 2 ? Promise.reject(unknown) : Promise.resolve('sent');
    }, isUnknown, (retry) => { pauses.push(retry); return Promise.resolve(); });
    expect(result).toBe('sent');
    expect(attempts).toEqual([0, 1, 2]);
    expect(pauses).toEqual([0, 1]);
  });

  it('stops at once on an answered failure', async () => {
    let attempts = 0;
    await expect(retryUnknownSend(() => { attempts += 1; return Promise.reject(answered); }, isUnknown, () => Promise.resolve()))
      .rejects.toBe(answered);
    expect(attempts).toBe(1);
  });

  it('gives up after the retries are spent', async () => {
    let attempts = 0;
    await expect(retryUnknownSend(() => { attempts += 1; return Promise.reject(unknown); }, isUnknown, () => Promise.resolve()))
      .rejects.toBe(unknown);
    expect(attempts).toBe(SEND_RETRIES + 1);
  });
});
