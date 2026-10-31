import { EventEmitter } from 'node:events';
import type { Page } from '@playwright/test';
import { expect, test, vi } from 'vitest';
import { captureFirstMessageErrors } from '../../e2e/helpers/first-message-errors.js';

function fixture() {
  const events = new EventEmitter();
  const url = 'http://localhost/api/cards/created/planner/run?contract=conversation_v1';
  const errors = captureFirstMessageErrors(events as unknown as Page, () => url);
  const body = JSON.stringify({ error: 'conversation runtime changed while reading', code: 'conflict' });
  const diagnostic = 'Failed to load resource: the server responded with a status of 409 (Conflict)';
  const consoleError = (text = diagnostic, location = url) => events.emit('console', {
    type: () => 'error', text: () => text, location: () => ({ url: location }),
  });
  const response = (overrides: { url?: string; method?: string; status?: number; text?: () => Promise<string> } = {}) => {
    events.emit('response', { url: () => overrides.url ?? url, status: () => overrides.status ?? 409,
      request: () => ({ method: () => overrides.method ?? 'GET' }), text: overrides.text ?? (() => Promise.resolve(body)) });
  };
  return { events, errors, response, consoleError, url, body, diagnostic };
}

test('allows a declared conflict even when its console event arrives first', async () => {
  const { errors, response, consoleError } = fixture();
  consoleError();
  response();
  expect(await errors()).toEqual([]);
});

test.each([
  { name: 'wrong code', body: { error: 'conversation runtime changed while reading', code: 'other' } },
  { name: 'wrong reason', body: { error: 'conversation archived', code: 'conflict' } },
  { name: 'missing reason', body: { code: 'conflict' } },
  { name: 'extra fields', body: { error: 'conversation runtime changed while reading', code: 'conflict', unexpected: true } },
  { name: 'null body', body: null },
])('rejects a 409 with $name', async ({ body }) => {
  const { errors, response, consoleError, diagnostic } = fixture();
  consoleError();
  response({ text: () => Promise.resolve(JSON.stringify(body)) });
  expect(await errors()).toEqual([expect.stringContaining('→ 409:'), diagnostic]);
});

test.each([
  { name: 'other conversation', url: 'http://localhost/api/cards/other/planner/run?contract=conversation_v1' },
  { name: 'other endpoint', url: 'http://localhost/api/cards/created/harness/items?contract=conversation_v1' },
  { name: 'other contract', url: 'http://localhost/api/cards/created/planner/run?contract=other' },
  { name: 'other origin', url: 'http://other/api/cards/created/planner/run?contract=conversation_v1' },
  { name: 'POST', method: 'POST' },
  { name: 'server failure', status: 500 },
])('rejects a matching body on $name', async overrides => {
  const { errors, response } = fixture();
  response(overrides);
  expect(await errors()).toEqual([expect.stringContaining(`→ ${overrides.status ?? 409}:`)]);
});

test('rejects malformed and unreadable bodies without an unhandled rejection', async () => {
  const { errors, response } = fixture();
  response({ text: () => Promise.resolve('not JSON') });
  response({ text: () => Promise.reject(new Error('cancelled')) });
  expect(await errors()).toEqual([expect.stringContaining('not JSON'), expect.stringContaining('<unreadable body>')]);
});

test('waits for response bodies before deciding which console errors are allowed', async () => {
  const { errors, response, consoleError, body } = fixture();
  let resolveBody!: (body: string) => void;
  const bodyPromise = new Promise<string>(resolve => { resolveBody = resolve; });
  response({ text: () => bodyPromise });
  consoleError();
  const resolved = vi.fn();
  const result = errors().then(value => { resolved(); return value; });
  await Promise.resolve();
  expect(resolved).not.toHaveBeenCalled();
  resolveBody(body);
  expect(await result).toEqual([]);
});

test('does not let a declared conflict mask other console or page errors', async () => {
  const { events, errors, response, consoleError, diagnostic, url } = fixture();
  response();
  consoleError();
  consoleError(); // No second body-validated response to account for this resource error.
  consoleError('application error');
  consoleError(diagnostic, `${url}&other=1`);
  events.emit('pageerror', new Error('uncaught exception'));
  expect(await errors()).toEqual([diagnostic, 'application error', diagnostic, 'uncaught exception']);
});

test('does not spend a validated response on a 500 diagnostic at the same URL', async () => {
  const { errors, response, consoleError } = fixture();
  response();
  const unexpected = 'Failed to load resource: the server responded with a status of 500 (Internal Server Error)';
  consoleError(unexpected);
  consoleError();
  expect(await errors()).toEqual([unexpected]);
});
