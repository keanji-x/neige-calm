import { expect, test } from '@playwright/test';
import { captureFirstMessageErrors } from './helpers/first-message-errors.js';

test('accepts the declared run-read conflict and its Chromium resource error', async ({ page }) => {
  const origin = 'http://first-message.test';
  const runUrl = `${origin}/api/cards/created/planner/run?contract=conversation_v1`;
  await page.route(`${origin}/**`, route => route.request().url() === `${origin}/`
    ? route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Errors</title>' })
    : route.fulfill({ status: 409, contentType: 'application/json', body: JSON.stringify({
      error: 'conversation runtime changed while reading', code: 'conflict',
    }) }));
  const errors = captureFirstMessageErrors(page, () => runUrl);
  await page.goto(origin);
  const consoleError = page.waitForEvent('console', message => message.type() === 'error');
  await page.evaluate(async url => { await (await fetch(url)).text(); }, runUrl);
  await consoleError;
  expect(await errors()).toEqual([]);
});
