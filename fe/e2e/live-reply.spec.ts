import { expect, test } from '@playwright/test';

import { createArea, createTrack } from './helpers/seed.js';

test.use({ contextOptions: { reducedMotion: 'reduce' } });

/* #1923 S2: the fixture app-server streams a reply scripted by a line of the message
 * (`osc-probe-child`, `fake-reply-hold:`) and holds the turn open until it is interrupted. */
test('shows a streamed reply before its turn ends, and the same text once stored', async ({ page, request }) => {
  const area = await createArea(request, `Live reply ${Date.now()}`);
  try {
    const track = await createTrack(request, area.id);
    await page.goto(`/next/track/${track.id}`);
    const opener = page.getByRole('button', { name: 'Conversation Planner' });
    await opener.click();
    const composer = page.getByRole('combobox', { name: 'Message' });
    await expect(composer).toHaveAttribute('contenteditable', 'true');
    await composer.fill('fake-reply-hold: Streamed |before |the end');
    await composer.press('Enter');

    const transcript = page.locator('[data-nc-thread]');
    const reply = transcript.locator('[data-nc-turn="agent"]');
    /* The turn is held open, so this text can only be the live copy. */
    await expect(reply).toHaveText('Streamed before the end');
    await expect(page.getByRole('button', { name: 'Stop' })).toBeVisible();

    await page.getByRole('button', { name: 'Stop' }).click();
    await expect(page.locator('[data-nc-current-meta]')).toContainText('Interrupted');
    await expect(reply).toHaveCount(1);
    await expect(reply).toHaveText('Streamed before the end');

    /* After a refresh only the stored `_partial` row can draw it. */
    await page.reload();
    await expect(opener.or(transcript).first()).toBeVisible();
    if (await opener.isVisible()) await opener.click();
    await expect(page.locator('[data-nc-current-meta]')).toContainText('Interrupted');
    await expect(reply).toHaveCount(1);
    await expect(reply).toHaveText('Streamed before the end');
  } finally {
    await request.delete(`/api/areas/${area.id}`);
  }
});
