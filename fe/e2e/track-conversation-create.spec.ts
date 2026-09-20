/*
 * Starting a conversation from a track page, against the real kernel over real HTTP. 201 is
 * required, not tolerated: CI runs the `osc-probe-child` app-server fixture so the mint can succeed.
 */

import { expect, test, type Page, type Request } from '@playwright/test';

import { createArea, createTrack } from './helpers/seed.js';

const createdAreaIds: string[] = [];

function captureBrowserErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
  page.on('pageerror', (error) => errors.push(error.message));
  return errors;
}

/** Every POST the page made to a Track conversations endpoint. */
function conversationCreates(requests: Request[]): Request[] {
  return requests.filter((request) => request.method() === 'POST'
    && /\/conversations$/.test(new URL(request.url()).pathname));
}

/** Every GET **the page** made to this track's conversations endpoint. */
function conversationReads(requests: Request[], trackId: string): Request[] {
  return requests.filter((request) => request.method() === 'GET'
    && new URL(request.url()).pathname === `/api/tracks/${trackId}/conversations`);
}

/** The rows the conversation list is rendering, by their accessible names. */
function conversationRows(page: Page) {
  return page.getByRole('button', { name: /^Conversation / });
}

test.beforeEach(() => { createdAreaIds.length = 0; });
test.afterEach(async ({ request }) => {
  for (const id of createdAreaIds) await request.delete(`/api/areas/${id}`);
  createdAreaIds.length = 0;
});

test('starts a conversation from a track page and sends the first message to that track', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  const track = await createTrack(request, area.id, `FE e2e conversation track ${Date.now()}`);

  const requests: Request[] = [];
  page.on('request', (pending) => requests.push(pending));

  await page.goto(`/next/track/${track.id}`);

  // The request array is the browser's own traffic; `request.get` below is Playwright's and never appears in it.
  await expect(page.getByRole('button', { name: 'Conversation Planner' })).toBeVisible();
  expect(
    conversationReads(requests, track.id).length,
    'the page must read its conversations from GET /api/tracks/{id}/conversations',
  ).toBeGreaterThan(0);
  const seeded = await request.get(`/api/tracks/${track.id}/conversations`);
  expect(seeded.ok()).toBe(true);
  expect(await seeded.json() as unknown[]).toEqual([]);
  await expect(conversationRows(page)).toHaveCount(1);

  await page.getByRole('button', { name: 'New conversation' }).click();
  await expect(page.getByRole('complementary', { name: 'Untitled' })).toBeVisible();

  expect(conversationCreates(requests)).toHaveLength(0);

  const message = 'what does this track do?';
  const composer = page.getByRole('combobox', { name: 'Message' });
  await composer.click();
  await composer.fill(message);
  const [created] = await Promise.all([
    page.waitForResponse((response) => response.request().method() === 'POST'
      && new URL(response.url()).pathname === `/api/tracks/${track.id}/conversations`),
    composer.press('Enter'),
  ]);

  const posts = conversationCreates(requests);
  expect(posts).toHaveLength(1);
  const post = posts[0];
  expect(new URL(post.url()).pathname).toBe(`/api/tracks/${track.id}/conversations`);
  expect(post.postDataJSON()).toEqual({ text: message });
  // Without this header the kernel 400s, and a second attempt would mint a second conversation.
  expect(await post.headerValue('idempotency-key')).toMatch(/[0-9a-f-]{36}/);

  expect(created.status(), `create failed: ${await created.text()}`).toBe(201);
  const conversation = await created.json() as { id: string; trackId: string; kind: string };
  expect(conversation.trackId).toBe(track.id);
  expect(conversation.kind).toBe('track-assistant');

  // The only assertion here that fails if the mint writes a card the list predicate does not match.
  const listed = await request.get(`/api/tracks/${track.id}/conversations`);
  expect(listed.ok()).toBe(true);
  expect(await listed.json() as { id: string }[]).toEqual([
    expect.objectContaining({ id: conversation.id, trackId: track.id }),
  ]);

  // The drawer replaces the list while open, so the list is counted after closing it.
  /* Located by the control only the drawer has — never by a name that depends on the thing under test. */
  const drawer = page.locator('[role="complementary"]')
    .filter({ has: page.getByRole('button', { name: 'Close conversation' }) });
  await expect(drawer).toBeVisible();

  /* The adopted drawer is named from the first thing said, which is a server row. Identity is asked
   * of the browser: an open drawer polls the card it shows, so the page's own traffic names it. */
  await expect(page.getByRole('complementary', { name: 'Untitled' })).toHaveCount(0);
  await expect(page.getByRole('complementary', { name: message })).toBeVisible();
  await expect
    .poll(() => requests.some((pending) => pending.method() === 'GET'
      && new URL(pending.url()).pathname === `/api/cards/${conversation.id}/harness/items`))
    .toBe(true);

  /* The kernel writes the sentence to the transcript when the queue drains it, before `turn/start`;
   * the fixture emits no items, so the row read back is the kernel's own (`turn_id` still null). */
  await expect(drawer.locator('[data-nc-turn="you"]')).toHaveText(message);
  await expect(drawer.locator('[data-nc-thread-empty]')).toHaveCount(0);
  const items = await request.get(`/api/cards/${conversation.id}/harness/items`);
  expect(items.ok()).toBe(true);
  const rows = await items.json() as {
    item_type: string | null; method: string; turn_id: string | null; params: string;
    input_segments?: { presentation: string; text: string }[];
  }[];
  expect(
    rows.map((row) => [row.item_type, row.method, row.turn_id]),
    'the sentence is readable as the kernel\'s own row before any echo — that is the whole point (#1625 P2)',
  ).toEqual([['userMessage', 'item/completed', null]]);
  expect(rows[0]?.input_segments?.[0]?.text).toContain(message);
  expect((JSON.parse(rows[0]?.params ?? '{}') as { _projection?: unknown })._projection).toBe(true);

  await page.getByRole('button', { name: 'Close conversation' }).click();
  await expect(conversationRows(page)).toHaveCount(2);

  // A late duplicate POST lands after `waitForResponse` returned; give it a bounded moment, then count again.
  await page.waitForTimeout(1_000);
  expect(
    conversationCreates(requests),
    'the first message must mint once, and still once after the interaction settles',
  ).toHaveLength(1);
  expect(await (await request.get(`/api/tracks/${track.id}/conversations`)).json() as unknown[])
    .toHaveLength(1);
  expect(errors).toEqual([]);
});

/* The endpoint's own guards, straight over HTTP; these do not depend on codex. */
test('the track conversations endpoint refuses a create it cannot make retryable', async ({ request }) => {
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  const track = await createTrack(request, area.id, `FE e2e conversation guards ${Date.now()}`);

  const noKey = await request.post(`/api/tracks/${track.id}/conversations`, { data: { text: 'hello' } });
  expect(noKey.status(), await noKey.text()).toBe(400);

  const blank = await request.post(`/api/tracks/${track.id}/conversations`, {
    headers: { 'Idempotency-Key': crypto.randomUUID() },
    data: { text: '   ' },
  });
  expect(blank.status(), await blank.text()).toBe(400);

  const missing = await request.post('/api/tracks/00000000000000000000000000000000/conversations', {
    headers: { 'Idempotency-Key': crypto.randomUUID() },
    data: { text: 'hello' },
  });
  expect(missing.status(), await missing.text()).toBe(404);
});
