import { expect, test } from '@playwright/test';

import { createArea, createTrack } from './helpers/seed.js';

test.use({ contextOptions: { reducedMotion: 'reduce' } });

// Real app and isolated kernel, with a controlled rejection at the HTTP edge.
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
    /*
     * #1552 — the same words can render in two places: the transcript bubble
     * and the "Queued messages" list. Every assertion below names the carrier
     * it is actually claiming something about, so a match can never stand in
     * for the other one. `data-nc-thread` / `data-nc-turn` are attributes the
     * transcript sets on purpose (`features/chat/thread/public.tsx`), unlike
     * the hashed CSS-module classes, which change on every build.
     *
     * Here the send was REFUSED (429), so the kernel never accepted it and no
     * queue entry exists — the transcript is the only carrier, and it is the
     * one the claim ("the words are retained") is about.
     */
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

test('explains a wedged conversation and carries its draft into recovery', async ({ page, request }, testInfo) => {
  const area = await createArea(request, `Stalled recovery ${Date.now()}`);
  try {
    const track = await createTrack(request, area.id);
    let phase = 'turn_running';
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
    await expect(page.getByRole('alert')).toContainText('This conversation is stuck');
    await expect(page.getByLabel('Working', { exact: true })).toHaveCount(0);
    await composer.press('Enter');
    await expect(composer).toHaveText('Keep this unsent draft');
    await page.screenshot({ path: testInfo.outputPath('stalled-desktop.png'), fullPage: true, animations: 'disabled' });
    await page.getByRole('button', { name: 'Start a new conversation' }).click();
    await expect(page.getByRole('complementary', { name: 'Untitled' })).toBeVisible();
    await expect(composer).toHaveAttribute('contenteditable', 'true');
    await expect(composer).toHaveText('Keep this unsent draft');
  } finally {
    await request.delete(`/api/areas/${area.id}`);
  }
});

test('keeps a lost acknowledgement uncertain and asks before resending', async ({ page, request }, testInfo) => {
  const area = await createArea(request, `Uncertain recovery ${Date.now()}`);
  try {
    const track = await createTrack(request, area.id);
    let attempts = 0;
    let accepted = false;
    await page.route('**/api/cards/*/planner/input', async (route) => {
      attempts += 1;
      const response = await route.fetch();
      accepted = response.ok();
      await route.abort('connectionreset');
    });
    await page.goto(`/next/track/${track.id}`);
    await page.getByRole('button', { name: 'Conversation Planner' }).click();
    const composer = page.getByRole('combobox', { name: 'Message' });
    await expect(composer).toHaveAttribute('contenteditable', 'true');
    await composer.fill('Keep this uncertain message');
    await composer.press('Enter');
    /*
     * The kernel accepted the input; the browser lost the answer.
     *
     * Since #1625 P2 the kernel writes the sentence to the transcript the
     * moment the queue drains it into a turn — before the app-server has said
     * anything, and CI's `osc-probe-child` fixture never says anything — so
     * the page's next read shows the words as a server row. A matching row is
     * a review hint, never a delivery receipt (#1505 F5: a stale read can
     * reveal an old equal message), so the attempt stays uncertain and the
     * reader is asked to look; only they may dismiss it. Before P2 the read
     * answered nothing here and the page showed the plain "Delivery is
     * unconfirmed" error with a `Check delivery` remedy; that state is now
     * reachable only while the kernel has not drained the queue, which this
     * stack does within the same second.
     */
    await expect(page.getByRole('status').filter({ hasText: 'Delivery is still unconfirmed' })).toBeVisible();
    await expect(page.getByText('A matching message is visible.', { exact: false })).toBeVisible();
    expect(accepted).toBe(true);
    expect(attempts).toBe(1);
    /*
     * #1552 — scoped to the transcript, which is the carrier this line is about:
     * the words the reader typed are on screen after the transport lost the
     * acknowledgement — once, as the server's row, not beside a failed echo.
     */
    const transcript = page.locator('[data-nc-thread]');
    await expect(transcript.getByText('Keep this uncertain message', { exact: true })).toBeVisible();
    await expect(page.getByRole('alert')).toHaveCount(0);
    await expect(page.getByText('Transport request failed', { exact: false })).toHaveCount(0);
    await expect(composer).toHaveAttribute('contenteditable', 'false');
    await page.setViewportSize({ width: 390, height: 844 });
    await page.screenshot({ path: testInfo.outputPath('uncertain-mobile.png'), fullPage: true, animations: 'disabled' });
    // Resending is offered, and it asks first: the kernel may already hold it.
    await page.getByRole('button', { name: 'Send again…' }).click();
    const confirmation = page.getByRole('dialog', { name: 'Send this message again?' });
    await expect(confirmation).toContainText('may already have arrived');
    await confirmation.getByRole('button', { name: 'Cancel' }).click();
    expect(attempts).toBe(1);
    await page.setViewportSize({ width: 1440, height: 960 });
    await page.screenshot({ path: testInfo.outputPath('matching-review-desktop.png'), fullPage: true, animations: 'disabled' });
    // The reader looked. That, and nothing the server said, clears the review.
    await page.getByRole('button', { name: 'I’ve checked' }).click();
    await expect(page.getByText('A matching message is visible.', { exact: false })).toHaveCount(0);
    await expect(composer).toHaveAttribute('contenteditable', 'true');
    expect(attempts).toBe(1);
  } finally {
    await request.delete(`/api/areas/${area.id}`);
  }
});
