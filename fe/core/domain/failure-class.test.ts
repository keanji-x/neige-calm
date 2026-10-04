import { describe, expect, it } from 'vitest';

import type { ApiFailure } from '../api/types.js';
import { CONVERSATION_CREATE_FAILURES, PLANNER_QUEUE_WRITE_FAILURES } from './conversation.js';
import { SEND_FAILURES } from './conversation-delivery.js';
import { classifyFailure, type FailureTable } from './failure-class.js';

const http = (status: number, code = 'http_error', message = 'answered'): ApiFailure =>
  ({ kind: 'http', status, code, message, body: { error: message, code } });
const unauthorized: ApiFailure = { kind: 'unauthorized', status: 401, code: 'unauthorized', message: 'signed out' };
const transport: ApiFailure = { kind: 'transport', message: 'dropped' };
const decode: ApiFailure = { kind: 'decode', message: 'malformed' };

/** Each route table against every failure kind: 401, each listed status and code, a lost answer, an unreadable one, none. */
const cases: ReadonlyArray<readonly [string, FailureTable<string>, ReadonlyArray<readonly [ApiFailure | null, string]>]> = [
  ['POST /planner/input', SEND_FAILURES, [
    [http(400), 'rejected'], [http(403), 'rejected'], [http(404), 'rejected'], [http(413), 'rejected'],
    [http(422), 'rejected'], [http(429), 'rejected'], [unauthorized, 'rejected'],
    [http(409, 'planner_harness_dormant'), 'refused'], [http(409, 'planner_harness_runtime_superseded'), 'refused'],
    /* An Edit's replace refused before any write (#2043): final, the server's reason is shown. */
    [http(409, 'planner_turn_not_replaceable'), 'refused'],
    /* The code decides before the status: an answer naming one of these never wrote. */
    [http(400, 'planner_harness_dormant'), 'refused'],
    [http(409, 'conflict'), 'unknown'], [http(408), 'unknown'], [http(500), 'unknown'], [http(502), 'unknown'],
    [http(503, 'service_unavailable'), 'unknown'], [http(504), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['DELETE / POST …/steer on a queued entry', PLANNER_QUEUE_WRITE_FAILURES, [
    [http(409, 'planner_input_stale'), 'stale'], [http(404, 'not_found'), 'gone'],
    [http(409, 'planner_steer_no_running_turn'), 'not_running'], [http(409, 'planner_steer_unknown_outcome'), 'unanswered'],
    /* A code is read only with the status it comes with. */
    [http(500, 'planner_steer_unknown_outcome'), 'failed'], [http(404, 'planner_input_stale'), 'gone'],
    [http(409, 'conflict'), 'failed'], [http(400), 'failed'], [unauthorized, 'failed'],
    [transport, 'failed'], [decode, 'failed'], [null, 'failed'],
  ]],
  ['POST /tracks/{id}/conversations', CONVERSATION_CREATE_FAILURES, [
    [http(409, 'idempotency_key_exhausted'), 'exhausted'], [http(404, 'not_found'), 'gone'],
    [http(503, 'service_unavailable'), 'unavailable'], [http(400, 'bad_request'), 'blocked'],
    [http(409, 'conflict', 'Idempotency-Key already used with different payload'), 'stale-payload'],
    [http(409, 'conflict', 'card already exists'), 'exists'],
    [http(500), 'retry'], [http(403), 'retry'], [unauthorized, 'retry'],
    [transport, 'retry'], [decode, 'retry'], [null, 'retry'],
  ]],
];

describe.each(cases)('classifying a failed %s', (_route, table, expected) => {
  it.each(expected)('reads %o as %s', (failure, kind) => {
    expect(classifyFailure(failure, table)).toBe(kind);
  });
});

describe('classifyFailure', () => {
  const table: FailureTable<'first' | 'second' | 'auth' | 'other'> = {
    rules: [
      { status: [409], code: 'a', message: 'needle', is: 'first' },
      { status: { from: 400, to: 409 }, is: 'second' },
    ],
    unauthorized: 'auth',
    otherwise: 'other',
  };

  it('takes the first rule whose every given field matches', () => {
    expect(classifyFailure(http(409, 'a', 'a needle here'), table)).toBe('first');
    expect(classifyFailure(http(409, 'a', 'no match'), table)).toBe('second');
    expect(classifyFailure(http(409, 'b', 'needle'), table)).toBe('second');
  });

  it('reads a status range inclusively at both ends', () => {
    expect(classifyFailure(http(400), table)).toBe('second');
    expect(classifyFailure(http(410), table)).toBe('other');
    expect(classifyFailure(http(399), table)).toBe('other');
  });

  it('never reads rules for a 401, however its code reads', () => {
    expect(classifyFailure({ ...unauthorized, code: 'a', message: 'needle' }, table)).toBe('auth');
  });
});
