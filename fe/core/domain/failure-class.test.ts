import { describe, expect, it } from 'vitest';

import type { ApiFailure } from '../api/types.js';
import {
  CONVERSATION_CREATE_FAILURES, PLANNER_ATTACHMENT_FAILURES, PLANNER_MODEL_FAILURES, PLANNER_QUEUE_WRITE_FAILURES,
} from './conversation.js';
import { SEND_FAILURES } from './conversation-delivery.js';
import { PLANNER_INTERRUPT_FAILURES } from './conversation-stop.js';
import { classifyFailure, NotSentError, refusalText, type FailureTable } from './failure-class.js';

const http = (status: number, code = 'http_error', message = 'answered'): ApiFailure =>
  ({ kind: 'http', status, code, message, body: { error: message, code } });
const unauthorized: ApiFailure = { kind: 'unauthorized', status: 401, code: 'unauthorized', message: 'signed out' };
const transport: ApiFailure = { kind: 'transport', message: 'dropped' };
const decode: ApiFailure = { kind: 'decode', message: 'malformed' };

/** Each route table against every failure kind: 401, each listed status and code, a lost answer, an unreadable one, none. */
const cases: ReadonlyArray<readonly [string, FailureTable<string>, ReadonlyArray<readonly [ApiFailure | null, string]>]> = [
  ['POST /planner/input', SEND_FAILURES, [
    /* A refusal of the body or the card: the same send would be refused again (#2068). */
    [http(400), 'refused'], [http(403), 'refused'], [http(404), 'refused'], [http(413), 'refused'], [http(422), 'refused'],
    /* Answered before handling, for a reason that can pass: the same send may be tried again. */
    [http(429), 'rejected'], [unauthorized, 'rejected'],
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
  ['PUT /planner/model', PLANNER_MODEL_FAILURES, [
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [http(404, 'not_found'), 'refused'],
    [http(422), 'refused'], [unauthorized, 'refused'],
    [http(409, 'conflict'), 'unknown'], [http(500, 'internal'), 'unknown'], [http(503, 'service_unavailable'), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['POST /planner/interrupt', PLANNER_INTERRUPT_FAILURES, [
    [http(403, 'forbidden'), 'refused'], [http(404, 'not_found'), 'refused'],
    [http(409, 'planner_harness_dormant'), 'refused'], [unauthorized, 'refused'],
    /* Only the dormant 409 is answered before a dispatch; any other may follow one. */
    [http(409, 'conflict'), 'unknown'], [http(400), 'unknown'], [http(500, 'internal'), 'unknown'],
    [http(503, 'service_unavailable'), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['POST /planner/attachments', PLANNER_ATTACHMENT_FAILURES, [
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [http(404, 'not_found'), 'refused'],
    [http(413, 'payload_too_large'), 'refused'], [unauthorized, 'refused'],
    [http(500, 'internal'), 'unknown'], [http(503, 'service_unavailable'), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
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

describe('refusalText', () => {
  const text = (failure: Parameters<typeof refusalText>[0]) => refusalText(failure, PLANNER_MODEL_FAILURES, 'Not changed.');

  it('shows a refusal in the server’s own words, or the fixed refusal when it gave none', () => {
    expect(text(http(400, 'bad_request', 'Claude is not ready'))).toBe('Claude is not ready');
    expect(text({ kind: 'http', status: 404, code: 'not_found', message: '' })).toBe('Not changed.');
  });

  /* #2068 item 27: stopped at admission, nothing left the browser. */
  it('reads a write that was not sent as refused, in the fixed words', () => {
    expect(text(new NotSentError(new Error('工作区恢复权限尚未准备好。')))).toBe('Not changed.');
  });

  it('gives no words for a failure that may have been stored', () => {
    expect(text(transport)).toBeNull();
    expect(text({ kind: 'transport', message: 'Request timed out.' })).toBeNull();
    expect(text(http(500, 'internal', 'model store unavailable'))).toBeNull();
    expect(text(null)).toBeNull();
  });
});
