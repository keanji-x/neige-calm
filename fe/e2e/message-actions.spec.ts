import { expect, test } from '@playwright/test';
import { createArea, createTrack } from './helpers/seed.js';

test.use({ contextOptions: { reducedMotion: 'reduce' } });

test('copies Markdown and explicitly regenerates while preserving a separate draft', async ({ page, request }) => {
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  const area = await createArea(request, `Message actions ${Date.now()}`);
  try {
    const track = await createTrack(request, area.id);
    const answer = 'Original answer\n\n```ts\nconst value = 1;\n```';
    await page.route('**/api/cards/*/harness/items?**', async (route) => {
      const response = await route.fetch();
      const cardId = new URL(route.request().url()).pathname.split('/')[3];
      const common = { worker_session_id: 'fixture', card_id: cardId, track_id: track.id, thread_id: 'thread',
        turn_id: 'turn', turn_error_text: null, item_uuid: null, created_at_ms: Date.now() };
      await route.fulfill({ response, json: [
        { ...common, id: 1, item_type: 'userMessage', method: 'item/completed',
          params: JSON.stringify({ item: { content: [{ text: 'Original prompt' }] } }) },
        { ...common, id: 2, item_type: 'agentMessage', method: 'item/completed',
          params: JSON.stringify({ item: { text: answer } }) },
        { ...common, id: 3, item_type: null, method: 'turn/completed',
          params: JSON.stringify({ id: 'turn', status: 'completed', error: null }) },
      ] });
    });
    const sent: unknown[] = [];
    await page.route('**/api/cards/*/planner/input', async (route) => {
      sent.push(route.request().postDataJSON()); await route.continue();
    });
    await page.goto(`/next/track/${track.id}`);
    await page.getByRole('button', { name: 'Conversation Planner' }).click();
    await page.getByRole('button', { name: 'Copy response', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Copied response' })).toBeVisible();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(answer);
    expect(sent).toHaveLength(0);
    const composer = page.getByRole('combobox', { name: 'Message' });
    await composer.fill('Keep this separate draft');
    await page.getByRole('button', { name: 'Regenerate response', exact: true }).click();
    await expect.poll(() => sent).toEqual([{ text: 'Original prompt' }]);
    await expect(composer).toHaveText('Keep this separate draft');
    await expect(page.locator('[data-nc-thread]').getByText('Original answer', { exact: true })).toBeVisible();
    expect(sent).toHaveLength(1);
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(composer).toHaveText('Keep this separate draft');
  } finally { await request.delete(`/api/areas/${area.id}`); }
});

test('edits the latest message in the composer and replaces it on Send, after the rewind', async ({ page, request }) => {
  const area = await createArea(request, `Message edit ${Date.now()}`);
  try {
    const track = await createTrack(request, area.id);
    const imageId = '0189bc3f-2b1a-4c7d-9e4f-1a2b3c4d5e6f.png';
    let removed = false;
    const rewinds: unknown[] = [];
    const sent: unknown[] = [];
    let releaseRewind!: () => void;
    const rewindHeld = new Promise<void>((done) => { releaseRewind = done; });
    const input = (cardId: string) => [{ presentation: 'user', text: 'User says:\nOriginal prompt',
      attachments: [{ id: imageId, contentType: 'image/png', size: 68, url: `/api/cards/${cardId}/planner/attachments/${imageId}` }] }];
    await page.route('**/api/cards/*/harness/items?**', async (route) => {
      const response = await route.fetch();
      const cardId = new URL(route.request().url()).pathname.split('/')[3];
      const common = { worker_session_id: 'fixture', card_id: cardId, track_id: track.id, thread_id: 'thread',
        turn_id: 'turn', turn_error_text: null, item_uuid: null, created_at_ms: Date.now() };
      await route.fulfill({ response, json: removed ? [] : [
        { ...common, id: 1, item_type: 'userMessage', method: 'item/completed', input_segments: input(cardId),
          params: JSON.stringify({ item: { content: [{ text: 'Original prompt' }] } }) },
        { ...common, id: 2, item_type: 'agentMessage', method: 'item/completed',
          params: JSON.stringify({ item: { text: 'Original answer' } }) },
        { ...common, id: 3, item_type: null, method: 'turn/completed',
          params: JSON.stringify({ id: 'turn', status: 'completed', error: null }) },
      ] });
    });
    await page.route(`**/planner/attachments/${imageId}`, (route) => route.fulfill({ contentType: 'image/png',
      body: Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==', 'base64') }));
    /* Held, as a Claude rewind's dry run holds it: the spinner is seen before it answers. */
    await page.route('**/api/cards/*/planner/rewind', async (route) => {
      rewinds.push(route.request().postDataJSON());
      await rewindHeld;
      removed = true;
      const cardId = new URL(route.request().url()).pathname.split('/')[3];
      await route.fulfill({ json: { card_id: cardId, turn_id: 'turn', input: input(cardId) } });
    });
    /* The fixture image was never uploaded, so the send is answered here rather than refused by the server. */
    await page.route('**/api/cards/*/planner/input', async (route) => {
      sent.push(route.request().postDataJSON());
      const cardId = new URL(route.request().url()).pathname.split('/')[3];
      await route.fulfill({ json: { card_id: cardId, worker_session_id: 'fixture' } });
    });
    await page.goto(`/next/track/${track.id}`);
    await page.getByRole('button', { name: 'Conversation Planner' }).click();
    await page.getByRole('button', { name: 'Edit message', exact: true }).click();
    const composer = page.getByRole('combobox', { name: 'Message' });
    await expect(composer).toHaveText('Original prompt');
    await expect(composer).toBeFocused();
    await expect(page.locator('[data-nc-edit-bar]')).toContainText('Editing message');
    await expect(page.locator('[data-nc-attachments] img')).toHaveAttribute('src', new RegExp(`${imageId}$`));
    await expect(page.locator('[data-nc-thread]').getByText('Original answer', { exact: true })).toBeVisible();
    await expect(page.locator('[data-nc-thread] [data-nc-turn="you"][data-nc-editing]')).toHaveText('Original prompt');
    await expect(page.locator('[data-nc-thread] [data-nc-turn-attachments][data-nc-editing] img')).toHaveCount(1);
    expect(rewinds).toEqual([]);
    await composer.press('End');
    await composer.pressSequentially(' Keep it short.');
    await page.getByRole('button', { name: 'Replace message' }).click();
    await expect(page.getByRole('button', { name: 'Sending…' })).toBeVisible();
    await expect.poll(() => rewinds).toEqual([{ turn_id: 'turn' }]);
    expect(sent).toEqual([]);
    releaseRewind();
    await expect.poll(() => sent).toEqual([{ text: 'Original prompt Keep it short.', attachments: [imageId] }]);
    await expect(page.getByRole('button', { name: 'Sending…' })).toHaveCount(0);
    await expect(page.locator('[data-nc-edit-bar]')).toHaveCount(0);
    await expect(page.locator('[data-nc-thread]').getByText('Original answer', { exact: true })).toHaveCount(0);
    expect(rewinds).toHaveLength(1);
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.locator('[data-nc-thread]').getByText('Original answer', { exact: true })).toHaveCount(0);
  } finally { await request.delete(`/api/areas/${area.id}`); }
});
