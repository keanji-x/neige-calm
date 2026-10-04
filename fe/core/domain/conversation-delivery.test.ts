import { describe, expect, it } from 'vitest';

import {
  failedConversationDelivery, KeyedSendFailure, retryUnknownSend, SEND_RETRIES, sendFailureKind,
  type SendFailureKind,
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

describe('what a failed attempt says about delivery', () => {
  it('is unknown after a lost answer, a server error or no answer at all', () => {
    expect(sendFailureKind({ kind: 'transport', message: 'dropped' })).toBe('unknown');
    expect(sendFailureKind({ kind: 'http', status: 503, code: 'service_unavailable', message: 'full' })).toBe('unknown');
    expect(sendFailureKind(null)).toBe('unknown');
  });

  it('is a refusal for the codes the server gives only before a binding exists', () => {
    expect(sendFailureKind({ kind: 'http', status: 409, code: 'planner_harness_dormant', message: 'reset' })).toBe('refused');
    expect(sendFailureKind({ kind: 'http', status: 409, code: 'planner_harness_runtime_superseded', message: 'again' })).toBe('refused');
  });

  it('is a rejection for an answer given before the request was handled', () => {
    expect(sendFailureKind({ kind: 'http', status: 400, code: 'bad_request', message: 'empty' })).toBe('rejected');
    expect(sendFailureKind({ kind: 'unauthorized', status: 401, code: 'session_expired', message: 'expired' })).toBe('rejected');
  });
});

describe('retrying an unknown send', () => {
  const failures = (kinds: readonly SendFailureKind[], unknown = false) => {
    const errors = kinds.map((kind) => new Error(kind));
    let attempts = 0;
    const run = retryUnknownSend(
      (index) => { attempts += 1; return Promise.reject(errors[Math.min(index, errors.length - 1)]); },
      (error) => (error as Error).message as SendFailureKind,
      () => Promise.resolve(),
      unknown,
    );
    return { run, attempts: () => attempts, errors };
  };

  it('sends again until an answer arrives', async () => {
    const attempts: number[] = [];
    const pauses: number[] = [];
    const result = await retryUnknownSend((index) => {
      attempts.push(index);
      return index < 2 ? Promise.reject(new Error('unknown')) : Promise.resolve('sent');
    }, () => 'unknown', (retry) => { pauses.push(retry); return Promise.resolve(); }, false);
    expect(result).toBe('sent');
    expect(attempts).toEqual([0, 1, 2]);
    expect(pauses).toEqual([0, 1]);
  });

  it('stops at once on an answered failure, and keeps what it said', async () => {
    const { run, attempts, errors } = failures(['rejected']);
    const failure = await run.catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(KeyedSendFailure);
    expect((failure as KeyedSendFailure).delivery).toBe('rejected');
    expect((failure as KeyedSendFailure).cause).toBe(errors[0]);
    expect(attempts()).toBe(1);
  });

  it.each(['rejected', 'refused', 'unknown'] as const)('keeps an unknown outcome unknown when a later attempt ends %s', async (last) => {
    const { run, attempts } = failures(last === 'unknown' ? ['unknown'] : ['unknown', last]);
    const failure = await run.catch((error: unknown) => error) as KeyedSendFailure;
    expect(failure.delivery).toBe('unknown');
    expect(attempts()).toBe(last === 'unknown' ? SEND_RETRIES + 1 : 2);
  });

  it.each(['rejected', 'refused'] as const)('keeps a resumed unknown op unknown when its first attempt ends %s', async (kind) => {
    const { run, attempts } = failures([kind], true);
    expect((await run.catch((error: unknown) => error) as KeyedSendFailure).delivery).toBe('unknown');
    expect(attempts()).toBe(1);
  });

  it('settles a refusal as refused while nothing was unknown', async () => {
    const { run } = failures(['refused']);
    expect((await run.catch((error: unknown) => error) as KeyedSendFailure).delivery).toBe('refused');
  });
});
