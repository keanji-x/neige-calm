import { describe, expect, it } from 'vitest';

import type { ApiFailure } from '../api/types.js';
import {
  KeyedSendFailure, retryUnknownSend, SEND_RETRIES, type SendFailureKind,
} from './conversation-delivery.js';

/** One failure the send table reads as each kind. */
const FAILURE_OF: Readonly<Record<SendFailureKind, ApiFailure>> = {
  unknown: { kind: 'transport', message: 'dropped' },
  refused: { kind: 'http', status: 409, code: 'planner_harness_dormant', message: 'reset' },
  rejected: { kind: 'http', status: 400, code: 'bad_request', message: 'empty' },
};

describe('retrying an unknown send', () => {
  const failures = (kinds: readonly SendFailureKind[], unknown = false) => {
    const errors = kinds.map((kind) => new Error(kind));
    let attempts = 0;
    const run = retryUnknownSend(
      (index) => { attempts += 1; return Promise.reject(errors[Math.min(index, errors.length - 1)]); },
      (error) => FAILURE_OF[(error as Error).message as SendFailureKind],
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
    }, () => null, (retry) => { pauses.push(retry); return Promise.resolve(); }, false);
    expect(result).toEqual({ sent: 'sent', everUnknown: true });
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

  it('tells a first-time answer from one that came after an unknown outcome', async () => {
    const answer = (unknown: boolean) => retryUnknownSend(() => Promise.resolve('sent'), () => null, () => Promise.resolve(), unknown);
    expect(await answer(false)).toEqual({ sent: 'sent', everUnknown: false });
    expect(await answer(true)).toEqual({ sent: 'sent', everUnknown: true });
  });

  it('settles a refusal as refused while nothing was unknown', async () => {
    const { run } = failures(['refused']);
    expect((await run.catch((error: unknown) => error) as KeyedSendFailure).delivery).toBe('refused');
  });
});
