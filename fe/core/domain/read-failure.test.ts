import { describe, expect, it } from 'vitest';

import type { ApiFailure } from '../api/types.js';
import { ApiError, classifyFailure } from './failure-class.js';
import { FILE_READ_FAILURES } from './fs.js';
import { RECHECK_TEXT } from './agent-providers.js';
import { CONNECTOR_CHECK_TEXT } from './plugins.js';
import { PROBE_FAILURES, probeFailureText, READ_FAILURES, readErrorText, readFailureOf, readFailureText } from './read-failure.js';

const http = (status: number, code = 'http_error', message = 'the server said why'): ApiFailure =>
  ({ kind: 'http', status, code, message, body: { error: message, code } });
const unauthorized: ApiFailure = { kind: 'unauthorized', status: 401, code: 'unauthorized', message: 'signed out' };
const transport: ApiFailure = { kind: 'transport', message: 'Failed to fetch' };
const decode: ApiFailure = { kind: 'decode', message: 'API response did not match its schema' };

const SENTENCE = 'Plugins are unavailable.';

describe('the shared read table', () => {
  it('is frozen', () => {
    expect(Object.isFrozen(READ_FAILURES) && Object.isFrozen(READ_FAILURES.rules)).toBe(true);
  });

  /* A 4xx is the server refusing this read for a reason; 401/408/429 and every other kind are not. */
  it.each([
    [http(400, 'bad_request'), 'refused'], [http(403, 'forbidden'), 'refused'], [http(404, 'not_found'), 'refused'],
    [http(409, 'conflict'), 'refused'], [http(413), 'refused'], [http(422), 'refused'], [http(499), 'refused'],
    [http(408), 'unavailable'], [http(429), 'unavailable'], [unauthorized, 'unavailable'],
    [http(500, 'internal'), 'unavailable'], [http(502), 'unavailable'], [http(503, 'service_unavailable'), 'unavailable'],
    [http(504), 'unavailable'], [http(399), 'unavailable'], [http(500, 'bad_request'), 'unavailable'],
    [transport, 'unavailable'], [decode, 'unavailable'], [null, 'unavailable'],
  ] as const)('reads %j as %s', (failure, expected) => {
    expect(classifyFailure(failure, READ_FAILURES)).toBe(expected);
  });
});

describe('the read sentence rule', () => {
  it('follows the fixed sentence with the server reason when the server refused', () => {
    expect(readFailureText(http(404, 'not_found', 'plugin calendar'), SENTENCE)).toBe('Plugins are unavailable. plugin calendar');
    expect(readFailureText(http(400, 'bad_request', '  path /x not found \n'), SENTENCE)).toBe('Plugins are unavailable. path /x not found');
  });

  it('shows only the fixed sentence for a refusal without a reason', () => {
    expect(readFailureText(http(403, 'forbidden', ''), SENTENCE)).toBe(SENTENCE);
    expect(readFailureText(http(403, 'forbidden', '   '), SENTENCE)).toBe(SENTENCE);
  });

  /* Server, transport and decode text is internals: it never reaches the screen, whatever it says. */
  it.each([
    http(500, 'internal', 'db: disk I/O error'), http(503, 'service_unavailable', 'Areas temporarily unavailable'),
    http(408, 'http_error', 'timed out'), http(429, 'http_error', 'slow down'), unauthorized, transport, decode,
  ])('shows only the fixed sentence for %j', (failure) => {
    expect(readFailureText(failure, SENTENCE)).toBe(SENTENCE);
  });

  it('reads a rejected read through its ApiError, and any other thrown value as unavailable', () => {
    expect(readErrorText(new ApiError(http(404, 'not_found', 'track w1')), SENTENCE)).toBe('Plugins are unavailable. track w1');
    expect(readErrorText(new ApiError(http(500, 'internal', 'boom')), SENTENCE)).toBe(SENTENCE);
    for (const thrown of [new Error('not found: track w1'), 'bad request: nope', null, undefined, { failure: http(404) }]) {
      expect(readFailureOf(thrown)).toBeNull();
      expect(readErrorText(thrown, SENTENCE)).toBe(SENTENCE);
    }
  });
});

/* The answers `routes/fs.rs` gives, by status and code only (see the table on `FILE_READ_FAILURES`). */
describe('the filesystem read classes', () => {
  it.each([
    [http(403, 'forbidden', 'permission denied reading /x'), 'denied'],
    [http(404, 'path_not_found', 'path /x not found'), 'missing'],
    /* A gone Track is not a missing path: restoring a path cannot bring it back. */
    [http(404, 'not_found', 'track w1'), 'gone'],
    /* The wording does not pick a class. */
    [http(400, 'bad_request', 'path /x not found'), 'other'],
    [http(400, 'bad_request', 'permission denied reading /x'), 'other'],
    /* Status and code together: neither alone. */
    [http(403, 'plugin_permission'), 'other'], [http(404, 'http_error'), 'other'], [http(500, 'forbidden'), 'other'],
    [http(400, 'path_not_found'), 'other'],
    [http(500, 'internal', 'fs /x: Permission denied'), 'other'],
    [unauthorized, 'other'], [transport, 'other'], [decode, 'other'], [null, 'other'],
  ] as const)('reads %j as %s', (failure, expected) => {
    expect(Object.isFrozen(FILE_READ_FAILURES) && Object.isFrozen(FILE_READ_FAILURES.rules)).toBe(true);
    expect(classifyFailure(failure, FILE_READ_FAILURES)).toBe(expected);
  });
});

/* #2175 S7: the one rule both read-only probes (Recheck, connector check) say their failures by. */
describe.each([['Recheck', RECHECK_TEXT], ['connector check', CONNECTOR_CHECK_TEXT]] as const)('a failed %s', (_probe, text) => {
  it('is frozen', () => {
    expect(Object.isFrozen(PROBE_FAILURES) && Object.isFrozen(PROBE_FAILURES.rules) && Object.isFrozen(text)).toBe(true);
  });

  it('says an answered failure in the server’s words, its field named', () => {
    expect(probeFailureText(new ApiError(http(502, 'mcp_setup_failed', 'HTTP 403: forbidden')), text)).toBe('HTTP 403: forbidden');
    expect(probeFailureText(new ApiError(http(500, 'internal', 'checks are wedged')), text)).toBe('checks are wedged');
    expect(probeFailureText(new ApiError({ kind: 'http', status: 400, code: 'bad_request', field: 'url', message: 'must use https' }), text))
      .toBe('url: must use https');
    expect(probeFailureText(new ApiError(http(400, 'bad_request', '  ')), text)).toBe(text.answered);
  });

  /* A 401 is the session's: the probe never ran, and the server's text would read as the probed server refusing. */
  it('says a probe that never ran, a signed-out session included, in its fixed words', () => {
    for (const error of [new ApiError(unauthorized), new ApiError(transport), new ApiError(decode), new Error('x'), null]) {
      expect(probeFailureText(error, text)).toBe(text.unfinished);
    }
  });
});
