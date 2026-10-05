import { describe, expect, it } from 'vitest';

import type { ApiFailure } from '../api/types.js';
import {
  CONVERSATION_CREATE_FAILURES, PLANNER_ATTACHMENT_FAILURES, PLANNER_MODEL_FAILURES, PLANNER_QUEUE_WRITE_FAILURES,
} from './conversation.js';
import { SEND_FAILURES } from './conversation-delivery.js';
import { PLANNER_INTERRUPT_FAILURES } from './conversation-stop.js';
import { AREA_CREATE_FAILURES, AREA_PATCH_FAILURES } from './area.js';
import { DISMISS_FAILURES } from './activity.js';
import {
  ApiError, classifyFailure, DELETE_FAILURES, DELETE_TEXT, NotSentError, refusalText, refusedText, writeFailureText, type FailureTable,
} from './failure-class.js';
import { LAUNCHPAD_ENSURE_FAILURES, REPORT_RESET_FAILURES } from './today.js';
import { CARD_CREATE_FAILURES, TRACK_CREATE_FAILURES, TRACK_PATCH_FAILURES } from './track.js';

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
    /* A key bound to another message can never be stored or replayed (#2068): final. */
    [http(409, 'idempotency_key_reused'), 'refused'], [http(400, 'idempotency_key_invalid'), 'refused'],
    /* Another request under the key was stored at that moment: a retry replays its answer. */
    [http(409, 'idempotency_key_concurrent'), 'unknown'],
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
    [http(409, 'idempotency_key_reused'), 'stale-payload'], [http(409, 'idempotency_key_concurrent'), 'retry'],
    /* Told apart by the code alone: the wording of a `conflict` decides nothing. */
    [http(409, 'conflict', 'Idempotency-Key already used with different payload'), 'exists'],
    [http(409, 'conflict', 'card already exists'), 'exists'],
    /* An invalid key can never be answered; a new one can. */
    [http(400, 'idempotency_key_invalid'), 'exhausted'],
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
  ['POST /tracks', TRACK_CREATE_FAILURES, [
    [http(409, 'idempotency_key_exhausted'), 'exhausted'],
    /* A key bound to another create, or to one the server can no longer compare: final. */
    [http(409, 'idempotency_key_reused'), 'key-reused'],
    /* An invalid key can never mint; only a fresh key goes anywhere. */
    [http(400, 'idempotency_key_invalid'), 'exhausted'], [http(400, 'bad_request'), 'rejected'],
    [http(403, 'forbidden'), 'rejected'], [http(404, 'not_found'), 'rejected'], [http(422), 'rejected'],
    [http(429), 'rejected'], [http(499), 'rejected'],
    /* May follow a commit: the request and its key are kept. */
    [http(409, 'idempotency_key_concurrent'), 'unconfirmed'], [http(409, 'conflict'), 'unconfirmed'],
    [http(408), 'unconfirmed'], [http(500, 'internal'), 'unconfirmed'], [http(503, 'service_unavailable'), 'unconfirmed'],
    [unauthorized, 'unconfirmed'], [transport, 'unconfirmed'], [decode, 'unconfirmed'], [null, 'unconfirmed'],
  ]],
  ['POST /areas', AREA_CREATE_FAILURES, [
    /* No retry under the key can succeed: the next Create mints a new one (#2068). */
    [http(409, 'idempotency_key_reused'), 'key-spent'], [http(400, 'idempotency_key_invalid'), 'key-spent'],
    [http(409, 'idempotency_key_exhausted'), 'key-spent'],
    [http(400, 'bad_request'), 'rejected'], [http(403), 'rejected'], [http(404), 'rejected'], [http(422), 'rejected'],
    [http(429), 'rejected'], [unauthorized, 'rejected'],
    /* A concurrent create or a lost answer: the request is kept for a retry. */
    [http(409, 'conflict'), 'unconfirmed'], [http(409, 'idempotency_key_concurrent'), 'unconfirmed'],
    [http(408), 'unconfirmed'], [http(500, 'internal'), 'unconfirmed'], [http(503, 'service_unavailable'), 'unconfirmed'],
    [transport, 'unconfirmed'], [decode, 'unconfirmed'], [null, 'unconfirmed'],
  ]],
  /* #2131: the non-chat writes. */
  ['DELETE by id (track, area, card, recipe)', DELETE_FAILURES, [
    /* The row is gone: what a retry after an unknown answer meets. */
    [http(404, 'not_found'), 'done'],
    [http(403, 'forbidden'), 'refused'], [http(409, 'conflict'), 'refused'], [http(409, 'terminal_disposal'), 'refused'],
    [unauthorized, 'refused'],
    [http(400), 'unknown'], [http(500, 'db_error'), 'unknown'], [http(503), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['PATCH /tracks/{id}', TRACK_PATCH_FAILURES, [
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [http(404, 'not_found'), 'refused'],
    [http(409, 'conflict'), 'refused'], [unauthorized, 'refused'],
    [http(413), 'unknown'], [http(500, 'db_error'), 'unknown'], [http(503), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['POST a card', CARD_CREATE_FAILURES, [
    [http(400, 'bad_request'), 'refused'], [http(403, 'plugin_permission'), 'refused'], [http(404, 'not_found'), 'refused'],
    [http(422, 'not_a_card_tool'), 'refused'], [unauthorized, 'refused'],
    [http(409, 'conflict'), 'unknown'], [http(500), 'unknown'], [http(502, 'tool_call_failed'), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['PATCH /areas/{id}', AREA_PATCH_FAILURES, [
    [http(400, 'bad_request'), 'refused'], [http(404, 'not_found'), 'refused'], [http(413), 'refused'],
    [http(422), 'refused'], [unauthorized, 'refused'],
    /* The server sends no 429; one would not be an answer this route documents. */
    [http(403), 'unknown'], [http(429), 'unknown'], [http(500, 'db_error'), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['POST /tracks/{id}/activity/dismissals', DISMISS_FAILURES, [
    /* The track is gone, and its notification with it. */
    [http(404, 'not_found'), 'done'],
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [http(422), 'refused'],
    [unauthorized, 'refused'],
    [http(409), 'unknown'], [http(500), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['POST /today/launchpad/ensure', LAUNCHPAD_ENSURE_FAILURES, [
    [http(403, 'forbidden'), 'refused'], [unauthorized, 'refused'],
    /* A failed assistant start answers 500 after the launchpad may have been made. */
    [http(400), 'unknown'], [http(404), 'unknown'], [http(500, 'internal'), 'unknown'], [http(503), 'unknown'],
    [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
  ]],
  ['POST /today/launchpad/report/reset', REPORT_RESET_FAILURES, [
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [http(404, 'not_found'), 'refused'],
    [unauthorized, 'refused'],
    /* A revision race with a writer: the reset may be tried again. */
    [http(409, 'conflict'), 'unknown'], [http(500), 'unknown'],
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
      { status: [409], code: 'a', is: 'first' },
      { status: { from: 400, to: 409 }, is: 'second' },
    ],
    unauthorized: 'auth',
    otherwise: 'other',
  };

  it('takes the first rule whose every given field matches', () => {
    expect(classifyFailure(http(409, 'a'), table)).toBe('first');
    expect(classifyFailure(http(400, 'a'), table)).toBe('second');
    expect(classifyFailure(http(409, 'b'), table)).toBe('second');
  });

  it('reads a status range inclusively at both ends', () => {
    expect(classifyFailure(http(400), table)).toBe('second');
    expect(classifyFailure(http(410), table)).toBe('other');
    expect(classifyFailure(http(399), table)).toBe('other');
  });

  it('never reads rules for a 401, however its code reads', () => {
    expect(classifyFailure({ ...unauthorized, code: 'a' }, table)).toBe('auth');
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

describe('writeFailureText', () => {
  const read = writeFailureText(DELETE_FAILURES, DELETE_TEXT);

  it('reads an answer that proves the intent holds as done: no sentence', () => {
    expect(read(new ApiError(http(404, 'not_found', 'track not found')))).toBeNull();
  });

  it('shows a refusal in the server’s words, or the fixed refusal when it gave none', () => {
    expect(read(new ApiError(http(409, 'conflict', 'a managed track cannot be deleted')))).toBe('a managed track cannot be deleted');
    expect(read(new ApiError({ kind: 'http', status: 403, code: 'forbidden', message: '' }))).toBe(DELETE_TEXT.refused);
  });

  it('reads a write that was not sent as refused, never as done or unknown', () => {
    expect(read(new NotSentError())).toBe(DELETE_TEXT.refused);
  });

  /* The raw failure text a lost, timed-out or unreadable answer carries is never shown. */
  it.each([
    new ApiError(transport), new ApiError({ kind: 'transport', message: 'Request timed out.' }), new ApiError(decode),
    new ApiError(http(500, 'db_error', 'database is locked')), new Error('anything else'), 'not an error',
  ])('shows the fixed unknown state for %o', (error) => {
    expect(read(error)).toBe(DELETE_TEXT.unknown);
  });
});

describe('refusedText', () => {
  it('is the server’s reason, or the fixed refusal when it gave none or nothing was sent', () => {
    expect(refusedText(http(429, 'rate_limited', 'slow down'), 'Not created.')).toBe('slow down');
    expect(refusedText({ kind: 'http', status: 400, code: 'bad_request', message: '' }, 'Not created.')).toBe('Not created.');
    expect(refusedText(new NotSentError(new Error('offline')), 'Not created.')).toBe('Not created.');
    expect(refusedText(null, 'Not created.')).toBe('Not created.');
  });
});
