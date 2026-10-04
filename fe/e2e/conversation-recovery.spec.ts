import { expect, test } from '@playwright/test';

import { createArea, createTrack } from './helpers/seed.js';

test.use({ contextOptions: { reducedMotion: 'reduce' } });

// No Codex execution is needed: the fixture app-server accepts the final send.
test('retains a rejected conversation message and retries it once', async ({ page, request }, testInfo) => {
  const area = await createArea(request, `Recovery check ${Date.now()}`);
  try {
    const track = await createTrack(request, area.id);
    let attempts = 0;
    let accepted = false;
    await page.route('**/api/cards/*/planner/input', async (route) => {
      attempts += 1;
      if (attempts === 1) {
        await route.fulfill({ status: 429, json: { code: 'rate_limited', error: 'Please try again in a moment.' } });
        return;
      }
      const response = await route.fetch();
      accepted = response.ok();
      await route.fulfill({ response });
    });
    await page.goto(`/next/track/${track.id}`);
    await page.getByRole('button', { name: 'Conversation Planner' }).click();
    const composer = page.getByRole('combobox', { name: 'Message' });
    await expect(composer).toHaveAttribute('contenteditable', 'true');
    const message = 'Retain this recovery check';
    /* The same words can render in the transcript bubble and the "Queued messages" list, so each
     * assertion names its carrier; `data-nc-thread` is set on purpose, unlike hashed CSS-module classes. */
    const transcript = page.locator('[data-nc-thread]');
    await composer.fill(message);
    await composer.press('Enter');
    await expect(page.getByRole('alert')).toContainText('Please try again');
    await expect(transcript.getByText(message, { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Close conversation' }).click();
    await page.getByRole('button', { name: 'Conversation Planner' }).click();
    await expect(transcript.getByText(message, { exact: true })).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath('rejected-desktop.png'), fullPage: true, animations: 'disabled' });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByRole('button', { name: 'Try again' })).toBeVisible();
    await expect.poll(async () => {
      const box = await page.getByRole('complementary', { name: 'Planner' }).boundingBox();
      return box !== null && box.x >= 0 && box.x + box.width <= 390;
    }).toBe(true);
    await page.screenshot({ path: testInfo.outputPath('rejected-mobile.png'), fullPage: true, animations: 'disabled' });
    await page.getByRole('button', { name: 'Try again' }).click();
    await expect(page.getByRole('alert')).toHaveCount(0);
    await expect.poll(() => accepted).toBe(true);
    expect(attempts).toBe(2);
  } finally {
    await request.delete(`/api/areas/${area.id}`);
  }
});

test('explains a paused conversation and retains its blocked draft', async ({ page, request }, testInfo) => {
  const area = await createArea(request, `Stalled recovery ${Date.now()}`);
  try {
    const track = await createTrack(request, area.id);
    let phase = 'turn_running';
    let sends = 0;
    await page.route('**/api/cards/*/planner/input', async (route) => { sends += 1; await route.continue(); });
    await page.route('**/api/cards/*/planner/run', async (route) => {
      const response = await route.fetch();
      const body = await response.json() as Record<string, unknown>;
      await route.fulfill({ response, json: { ...body, phase } });
    });
    await page.goto(`/next/track/${track.id}`);
    await page.getByRole('button', { name: 'Conversation Planner' }).click();
    const composer = page.getByRole('combobox', { name: 'Message' });
    await expect(composer).toHaveAttribute('contenteditable', 'true');
    await composer.fill('Keep this unsent draft');
    phase = 'wedged';
    await page.evaluate(() => window.dispatchEvent(new Event('visibilitychange')));
    const pause = page.locator('[data-nc-drawer-scroll]').getByRole('button', { name: 'Paused', exact: true });
    await expect(pause).toBeVisible();
    await expect(pause).toHaveAttribute('aria-expanded', 'false');
    await pause.click();
    await expect(page.locator('[data-nc-thread]').getByText('This conversation is stuck.', { exact: true })).toBeVisible();
    await expect(page.getByRole('alert')).toHaveCount(0);
    await expect(composer).toHaveAttribute('contenteditable', 'false');
    await expect(page.getByRole('complementary').locator('[data-nc-activity="working"]')).toHaveCount(0);
    await composer.press('Enter');
    await expect(composer).toHaveText('Keep this unsent draft');
    await page.screenshot({ path: testInfo.outputPath('stalled-desktop.png'), fullPage: true, animations: 'disabled' });
    await expect(page.getByRole('button', { name: 'Start a new conversation' })).toHaveCount(0);
    expect(sends).toBe(0);
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(pause).toBeVisible();
    await expect(composer).toHaveAttribute('contenteditable', 'false');
    await expect(composer).toHaveText('Keep this unsent draft');
    await page.screenshot({ path: testInfo.outputPath('stalled-mobile.png'), fullPage: true, animations: 'disabled' });
  } finally {
    await request.delete(`/api/areas/${area.id}`);
  }
});

test('sends a lost acknowledgement again under its key and keeps one message', async ({ page, request }, testInfo) => {
  const area = await createArea(request, `Lost acknowledgement ${Date.now()}`);
  try {
    const track = await createTrack(request, area.id);
    const keys: (string | undefined)[] = [];
    const answers: number[] = [];
    await page.route('**/api/cards/*/planner/input', async (route) => {
      keys.push(route.request().headers()['idempotency-key']);
      const response = await route.fetch();
      answers.push(response.status());
      /* The kernel stored the first request; only its answer is lost on the way back. */
      if (keys.length === 1) await route.abort('connectionreset');
      else await route.fulfill({ response });
    });
    await page.goto(`/next/track/${track.id}`);
    await page.getByRole('button', { name: 'Conversation Planner' }).click();
    const composer = page.getByRole('combobox', { name: 'Message' });
    await expect(composer).toHaveAttribute('contenteditable', 'true');
    const message = 'Deliver this message once';
    await composer.fill(message);
    await composer.press('Enter');
    /* Retried on its own, under the same key, and the kernel answers it from the first request. */
    await expect.poll(() => keys.length).toBe(2);
    expect(keys[0]).toBeTruthy();
    expect(keys[1]).toBe(keys[0]);
    expect(answers).toEqual([200, 200]);
    const transcript = page.locator('[data-nc-thread]');
    await expect(transcript.getByText(message, { exact: true })).toHaveCount(1);
    await expect(page.getByRole('alert')).toHaveCount(0);
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await page.screenshot({ path: testInfo.outputPath('lost-acknowledgement-desktop.png'), fullPage: true, animations: 'disabled' });
    /* What the server holds, with nothing optimistic left: one message. */
    await page.reload();
    /* The open conversation is restored by the reload, now named after its message. */
    await expect(page.getByRole('complementary', { name: message })).toBeVisible();
    await expect(transcript.getByText(message, { exact: true })).toHaveCount(1);
    await page.setViewportSize({ width: 390, height: 844 });
    await page.screenshot({ path: testInfo.outputPath('lost-acknowledgement-mobile.png'), fullPage: true, animations: 'disabled' });
    expect(keys).toHaveLength(2);
  } finally {
    await request.delete(`/api/areas/${area.id}`);
  }
});

for (const failedFirst of [false, true]) {
  test(`shows an unconfirmed stop without fabricating completion${failedFirst ? ' after a failed request' : ''}`, async ({ page, request }, testInfo) => {
    const area = await createArea(request, `Stop receipt ${Date.now()}`);
    try {
      const track = await createTrack(request, area.id);
      let stops = 0;
      let sends = 0;
      // The real harness is idle. A stale working snapshot reproduces a click
      // racing the response's end; the real interrupt route returns stopped:false.
      await page.route('**/api/cards/*/planner/run', async (route) => {
        const response = await route.fetch();
        const body = await response.json() as Record<string, unknown>;
        await route.fulfill({ response, json: { ...body, phase: 'turn_running' } });
      });
      await page.route('**/api/cards/*/planner/interrupt', async (route) => {
        stops += 1;
        if (failedFirst && stops === 1) {
          await route.fulfill({ status: 503, json: { code: 'service_unavailable', error: 'The connection is unavailable.' } });
        } else await route.continue();
      });
      page.on('request', (outgoing) => { if (outgoing.method() === 'POST' && outgoing.url().endsWith('/planner/input')) sends += 1; });
      await page.goto(`/next/track/${track.id}`);
      await page.getByRole('button', { name: 'Conversation Planner' }).click();
      const composer = page.getByRole('combobox', { name: 'Message' });
      await expect(composer).toHaveAttribute('contenteditable', 'true');
      await composer.fill('Keep this stop request draft');
      await page.getByRole('button', { name: 'Stop', exact: true }).click();
      if (failedFirst) {
        const failure = page.getByRole('button', { name: 'Stop failed', exact: true });
        await expect(failure).toBeVisible();
        await failure.click();
        await expect(page.getByText('The connection is unavailable.', { exact: true })).toBeVisible();
        expect(stops).toBe(1);
        await page.getByRole('button', { name: 'Stop', exact: true }).click();
      }
      const notice = page.locator('[data-nc-drawer-scroll]').getByRole('button', { name: 'Stop unconfirmed', exact: true });
      await expect(notice).toBeVisible();
      // Retrying preserves the user's open disclosure across status changes.
      await expect(notice).toHaveAttribute('aria-expanded', failedFirst ? 'true' : 'false');
      if (!failedFirst) await notice.click();
      await expect(page.getByText('The response may still be starting or may already have ended.', { exact: true })).toBeVisible();
      await expect(page.locator('[data-nc-turn-outcome]')).toHaveCount(0);
      await expect(page.getByRole('alert')).toHaveCount(0);
      await expect(composer).toHaveText('Keep this stop request draft');
      expect(stops).toBe(failedFirst ? 2 : 1);
      expect(sends).toBe(0);
      await page.setViewportSize({ width: 390, height: 844 });
      await expect(notice).toBeVisible();
      await expect(composer).toHaveText('Keep this stop request draft');
      await page.screenshot({ path: testInfo.outputPath('stop-unconfirmed-mobile.png'), fullPage: true, animations: 'disabled' });
    } finally {
      await request.delete(`/api/areas/${area.id}`);
    }
  });
}

for (const status of ['interrupted', 'failed'] as const) {
  test(`continues a ${status} response only after an explicit composer send`, async ({ page, request }) => {
    const area = await createArea(request, `Continuation ${Date.now()}`);
    try {
      const track = await createTrack(request, area.id);
      let sends = 0;
      await page.route('**/api/cards/*/harness/items?**', async (route) => {
        const response = await route.fetch();
        const cardId = new URL(route.request().url()).pathname.split('/')[3];
        await route.fulfill({ response, json: [{ id: 99, worker_session_id: 'fixture', card_id: cardId,
          track_id: track.id, thread_id: 'previous-thread', turn_id: 'previous-turn',
          item_uuid: null, item_type: null, method: 'turn/completed',
          params: JSON.stringify({ id: 'previous-turn', status, error: null }),
          turn_error_text: null, created_at_ms: Date.now() }] });
      });
      await page.route('**/api/cards/*/planner/input', async (route) => { sends += 1; await route.continue(); });
      await page.goto(`/next/track/${track.id}`);
      await page.getByRole('button', { name: 'Conversation Planner' }).click();
      const guidance = page.getByText('Send a message to continue.', { exact: true });
      await expect(guidance).not.toBeVisible();
      const disclosure = page.getByRole('button', { name: status === 'failed' ? /^Failed/ : /^Interrupted/ });
      await disclosure.click();
      await expect(guidance).toBeVisible();
      expect(sends).toBe(0);
      const composer = page.getByRole('combobox', { name: 'Message' });
      await composer.fill('Continue from the partial answer');
      // Reading details keeps the draft; closing a drawer has a separate draft lifecycle.
      await disclosure.click();
      await disclosure.click();
      await expect(composer).toHaveText('Continue from the partial answer');
      expect(sends).toBe(0);
      await page.setViewportSize({ width: 390, height: 844 });
      await expect(guidance).toBeVisible();
      await composer.press('Enter');
      await expect.poll(() => sends).toBe(1);
      await expect(page.locator('[data-nc-thread]').getByText('Continue from the partial answer', { exact: true })).toBeVisible();
      await expect(guidance).toHaveCount(0);
      expect(sends).toBe(1);
    } finally { await request.delete(`/api/areas/${area.id}`); }
  });
}
