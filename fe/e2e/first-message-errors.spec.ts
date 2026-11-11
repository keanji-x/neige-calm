import { EventEmitter } from 'node:events';
import { expect, test, type Page } from '@playwright/test';
import { captureFirstMessageErrors } from './helpers/first-message-errors.js';

test.describe('classifier', () => {
  async function fixture() {
    // Responses are emitted directly; nothing is routed.
    const events = Object.assign(new EventEmitter(), { route: async () => {}, unroute: async () => {} });
    const url = 'http://localhost/api/cards/created/planner/run?contract=conversation_v1';
    const errors = await captureFirstMessageErrors(events as unknown as Page, () => url);
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
    const { errors, response, consoleError } = await fixture();
    consoleError();
    response();
    expect(await errors()).toEqual([]);
  });

  for (const scenario of [
    { name: 'wrong code', body: { error: 'conversation runtime changed while reading', code: 'other' } },
    { name: 'wrong reason', body: { error: 'conversation archived', code: 'conflict' } },
    { name: 'missing reason', body: { code: 'conflict' } },
    { name: 'extra fields', body: { error: 'conversation runtime changed while reading', code: 'conflict', unexpected: true } },
    { name: 'null body', body: null },
  ]) test(`rejects a 409 with ${scenario.name}`, async () => {
    const { body } = scenario;
    const { errors, response, consoleError, diagnostic } = await fixture();
    consoleError();
    response({ text: () => Promise.resolve(JSON.stringify(body)) });
    expect(await errors()).toEqual([expect.stringContaining('→ 409:'), diagnostic]);
  });

  for (const overrides of [
    { name: 'other conversation', url: 'http://localhost/api/cards/other/planner/run?contract=conversation_v1' },
    { name: 'other endpoint', url: 'http://localhost/api/cards/created/harness/items?contract=conversation_v1' },
    { name: 'other contract', url: 'http://localhost/api/cards/created/planner/run?contract=other' },
    { name: 'other origin', url: 'http://other/api/cards/created/planner/run?contract=conversation_v1' },
    { name: 'POST', method: 'POST' },
    { name: 'server failure', status: 500 },
  ]) test(`rejects a matching body on ${overrides.name}`, async () => {
    const { errors, response } = await fixture();
    response(overrides);
    expect(await errors()).toEqual([expect.stringContaining(`→ ${overrides.status ?? 409}:`)]);
  });

  test('rejects malformed and unreadable bodies without an unhandled rejection', async () => {
    const { errors, response } = await fixture();
    response({ text: () => Promise.resolve('not JSON') });
    response({ text: () => Promise.reject(new Error('cancelled')) });
    expect(await errors()).toEqual([expect.stringContaining('not JSON'), expect.stringContaining('<unreadable body>')]);
  });

  test('waits for response bodies before deciding which console errors are allowed', async () => {
    const { errors, response, consoleError, body } = await fixture();
    let resolveBody!: (body: string) => void;
    const bodyPromise = new Promise<string>(resolve => { resolveBody = resolve; });
    response({ text: () => bodyPromise });
    consoleError();
    let resolved = false;
    const result = errors().then(value => { resolved = true; return value; });
    await Promise.resolve();
    expect(resolved).toBe(false);
    resolveBody(body);
    expect(await result).toEqual([]);
  });

  test('does not let a declared conflict mask other console or page errors', async () => {
    const { events, errors, response, consoleError, diagnostic, url } = await fixture();
    response();
    consoleError();
    consoleError(); // No second body-validated response to account for this resource error.
    consoleError('application error');
    consoleError(diagnostic, `${url}&other=1`);
    events.emit('pageerror', new Error('uncaught exception'));
    expect(await errors()).toEqual([diagnostic, 'application error', diagnostic, 'uncaught exception']);
  });

  test('does not spend a validated response on a 500 diagnostic at the same URL', async () => {
    const { errors, response, consoleError } = await fixture();
    response();
    const unexpected = 'Failed to load resource: the server responded with a status of 500 (Internal Server Error)';
    consoleError(unexpected);
    consoleError();
    expect(await errors()).toEqual([unexpected]);
  });
});

test('accepts the declared run-read conflict and its Chromium resource error', async ({ page }) => {
  const origin = 'http://first-message.test';
  const runUrl = `${origin}/api/cards/created/planner/run?contract=conversation_v1`;
  // Registered after the helper's own route, so this origin is answered here and never fetched.
  const errors = await captureFirstMessageErrors(page, () => runUrl);
  await page.route(`${origin}/**`, route => route.request().url() === `${origin}/`
    ? route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Errors</title>' })
    : route.fulfill({ status: 409, contentType: 'application/json', body: JSON.stringify({
      error: 'conversation runtime changed while reading', code: 'conflict',
    }) }));
  await page.goto(origin);
  const consoleError = page.waitForEvent('console', message => message.type() === 'error');
  await page.evaluate(async url => { await (await fetch(url)).text(); }, runUrl);
  await consoleError;
  expect(await errors()).toEqual([]);
});

test('classifies a run read the page aborts after its headers by the bytes the server sent', async ({ page, context }) => {
  // A real refusal at a run-read address: no card has this id.
  const path = '/api/cards/conv-00000000000000000000000000000000/planner/run?contract=conversation_v1';
  await page.goto('/api/version');
  const browserIdentity = await page.evaluate(async () => await (await fetch('/api/auth/whoami')).json() as unknown);
  const apiIdentity = await context.request.get('/api/auth/whoami');
  expect(await apiIdentity.json()).toEqual(browserIdentity);
  const served = await context.request.get(path, { maxRedirects: 0, maxRetries: 0 });
  expect(served.status()).toBeGreaterThanOrEqual(400);
  const body = await served.text();
  const errors = await captureFirstMessageErrors(page, () => undefined);
  // As a query cancellation does when an invalidation replaces a read whose headers have arrived.
  const status = await page.evaluate(async url => {
    const controller = new AbortController();
    const response = await fetch(url, { signal: controller.signal });
    controller.abort();
    return response.status;
  }, path);
  expect(status).toBe(served.status());
  expect((await errors()).filter(error => error.startsWith('GET ')))
    .toEqual([`GET ${new URL(path, page.url()).href} → ${status}: ${body}`]);
});


test('keeps caller routes after collecting errors', async ({ page }) => {
  const origin = 'http://first-message.test';
  let calls = 0;
  await page.route(`${origin}/**`, route => {
    calls++;
    return route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Caller route</title>' });
  });
  await page.goto(origin);
  const errors = await captureFirstMessageErrors(page, () => undefined);
  expect(await errors()).toEqual([]);
  await page.evaluate(async () => (await fetch('/caller-owned')).text());
  expect(calls).toBe(2);
});
