import { describe, expect, it } from 'vitest';

import type { ApiFailure } from '../api/types.js';
import { ApiError, classifyFailure, writeFailureText, type FailureTable } from './failure-class.js';
import { LOGIN_FAILURES, LOGIN_TEXT } from './login.js';
import {
  MOBILE_APPROVE_FAILURES, MOBILE_INVITATION_FAILURES, MOBILE_READ_FAILURES, MOBILE_READ_TEXT, MOBILE_REVOKE_FAILURES,
  MOBILE_STATE_FAILURES, MOBILE_WRITE_TEXT,
} from './mobile-access.js';

const http = (status: number, code = 'http_error', message = 'answered'): ApiFailure =>
  ({ kind: 'http', status, code, message, body: { error: message, code } });
const unauthorized: ApiFailure = { kind: 'unauthorized', status: 401, code: 'unauthorized', message: 'unauthorized' };
const transport: ApiFailure = { kind: 'transport', message: 'Transport request failed' };
const decode: ApiFailure = { kind: 'decode', message: 'API response did not match its schema' };
const lost: ReadonlyArray<readonly [ApiFailure | null, string]> = [
  [http(500, 'internal'), 'unknown'], [http(502), 'unknown'], [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
];

/** Each mobile route's table against the answers its handler gives (#2131 S3), a 401, and every kind of lost answer. */
const cases: ReadonlyArray<readonly [string, FailureTable<string>, ReadonlyArray<readonly [ApiFailure | null, string]>]> = [
  ['POST|DELETE /api/mobile/access, tailnet login|logout, DELETE /api/mobile/enrollments/{id}', MOBILE_STATE_FAILURES, [
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [unauthorized, 'refused'], ...lost,
  ]],
  ['POST /api/mobile/pairings, POST /api/mobile/enrollments', MOBILE_INVITATION_FAILURES, [
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'],
    /* Access is off, or the slot was cancelled while it was being created. */
    [http(409, 'conflict'), 'refused'], [http(409, 'idempotency_key_concurrent'), 'unknown'],
    [unauthorized, 'refused'], ...lost,
  ]],
  ['POST /api/mobile/pairings/{id}/approve', MOBILE_APPROVE_FAILURES, [
    /* No pending request: expired, never claimed, or unknown. A repeat of an approve that held answers 204. */
    [http(404, 'not_found'), 'refused'], [http(409, 'conflict'), 'refused'], [http(403, 'forbidden'), 'refused'],
    [http(404, 'http_error'), 'unknown'], [unauthorized, 'refused'], ...lost,
  ]],
  ['DELETE /api/mobile/devices/{id}', MOBILE_REVOKE_FAILURES, [
    /* The device is already gone: what revoke asked for. */
    [http(404, 'not_found'), 'done'], [http(403, 'forbidden'), 'refused'],
    [http(404, 'http_error'), 'unknown'], [unauthorized, 'refused'], ...lost,
  ]],
  ['GET /api/mobile/access, GET /api/mobile/enrollments', MOBILE_READ_FAILURES, [
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [unauthorized, 'refused'], ...lost,
  ]],
  ['POST /api/auth/login', LOGIN_FAILURES, [
    [unauthorized, 'credentials'], [http(400), 'refused'], [http(403), 'refused'], [http(422), 'refused'], ...lost,
  ]],
];

describe('mobile access failure tables', () => {
  it.each(cases)('%s', (_route, table, rows) => {
    expect(Object.isFrozen(table) && Object.isFrozen(table.rules)).toBe(true);
    for (const [failure, expected] of rows) expect([failure, classifyFailure(failure, table)]).toEqual([failure, expected]);
  });
});

describe('mobile access pane sentences', () => {
  const read = (table: FailureTable<'refused' | 'done' | 'unknown'>, failure: ApiFailure, text = MOBILE_WRITE_TEXT) =>
    writeFailureText(table, text)(new ApiError(failure));
  it('a refusal is the server reason, done says nothing, and unknown is the fixed sentence', () => {
    expect(read(MOBILE_APPROVE_FAILURES, http(404, 'not_found', 'not found: No pending pairing request')))
      .toBe('not found: No pending pairing request');
    expect(read(MOBILE_REVOKE_FAILURES, http(404, 'not_found', 'not found: No paired device'))).toBeNull();
    expect(read(MOBILE_REVOKE_FAILURES, http(403, 'forbidden', ''))).toBe(MOBILE_WRITE_TEXT.refused);
    for (const failure of [transport, decode, http(500)]) {
      expect(read(MOBILE_STATE_FAILURES, failure)).toBe(MOBILE_WRITE_TEXT.unknown);
      expect(read(MOBILE_READ_FAILURES, failure, MOBILE_READ_TEXT)).toBe(MOBILE_READ_TEXT.unknown);
    }
    expect(read(MOBILE_READ_FAILURES, http(400, 'bad_request', 'Private remote access could not initialize'), MOBILE_READ_TEXT))
      .toBe('Private remote access could not initialize');
    for (const sentence of [MOBILE_WRITE_TEXT, MOBILE_READ_TEXT, LOGIN_TEXT].flatMap((text) => [text.refused, text.unknown])) {
      expect(sentence).not.toMatch(/connect|network|offline|transport/i);
    }
  });
});
