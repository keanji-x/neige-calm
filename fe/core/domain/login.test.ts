import { describe, expect, it } from 'vitest';

import type { ApiFailure } from '../api/types.js';
import { classifyFailure } from './failure-class.js';
import { LOGIN_FAILURES, LOGIN_TEXT } from './login.js';

const http = (status: number, code = 'http_error', message = 'answered'): ApiFailure =>
  ({ kind: 'http', status, code, message, body: { error: message, code } });
const unauthorized: ApiFailure = { kind: 'unauthorized', status: 401, code: 'unauthorized', message: 'unauthorized' };
const transport: ApiFailure = { kind: 'transport', message: 'Transport request failed' };
const decode: ApiFailure = { kind: 'decode', message: 'API response did not match its schema' };

describe('POST /api/auth/login failure table', () => {
  it('reads each answer the login handler gives, a 401, and every kind of lost answer', () => {
    expect(Object.isFrozen(LOGIN_FAILURES) && Object.isFrozen(LOGIN_FAILURES.rules)).toBe(true);
    const rows: ReadonlyArray<readonly [ApiFailure | null, string]> = [
      [unauthorized, 'credentials'], [http(400), 'refused'], [http(403), 'refused'], [http(422), 'refused'],
      /* Repeated failures from one peer: refused before the credentials are read (#2132). */
      [http(429, 'login_throttled'), 'refused'],
      [http(500, 'internal'), 'unknown'], [http(502), 'unknown'], [transport, 'unknown'], [decode, 'unknown'], [null, 'unknown'],
    ];
    for (const [failure, expected] of rows) expect([failure, classifyFailure(failure, LOGIN_FAILURES)]).toEqual([failure, expected]);
  });

  it('neither sentence speaks of the connection', () => {
    for (const sentence of [LOGIN_TEXT.refused, LOGIN_TEXT.unknown]) {
      expect(sentence).not.toMatch(/connect|network|offline|transport/i);
    }
  });
});
