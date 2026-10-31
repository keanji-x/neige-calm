import type { ConversationPage } from '../core/api/generated/wire.ts';
/*
 * Starting a conversation from a track page, against the real kernel over real HTTP. 201 is
 * required, not tolerated: CI runs the `osc-probe-child` app-server fixture so the mint can succeed.
 */

import { expect, test, type Page, type Request } from '@playwright/test';

import { createArea, createTrack } from './helpers/seed.js';
import { captureFirstMessageErrors } from './helpers/first-message-errors.js';

const createdAreaIds: string[] = [];

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
  const errors = captureFirstMessageErrors(page, () => runUrl);
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
  expect(post.postDataJSON()).toEqual({ provider: 'codex', text: message });
  // Without this header the kernel 400s, and a second attempt would mint a second conversation.
  expect(await post.headerValue('idempotency-key')).toMatch(/[0-9a-f-]{36}/);

  expect(created.status(), `create failed: ${await created.text()}`).toBe(201);
  const conversation = await created.json() as { id: string; trackId: string; kind: string; provider: string };
  const runUrl = new URL(`/api/cards/${conversation.id}/planner/run?contract=conversation_v1`, page.url()).href;
  expect(conversation.trackId).toBe(track.id);
  expect(conversation.kind).toBe('track-assistant');
  expect(conversation.provider).toBe('codex');

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
      && new URL(pending.url()).pathname === `/api/cards/${conversation.id}/harness/items`
      && new URL(pending.url()).searchParams.get('contract') === 'conversation_v1'))
    .toBe(true);

  // The saved Human identity replaces the creation echo without relying on a provider echo.
  await expect(drawer.locator('[data-nc-thread]').getByText(message, { exact: true })).toHaveCount(1);
  await expect(drawer.locator('[data-nc-thread]').getByText(message, { exact: true })).toBeVisible();
  await expect(drawer.locator('[data-nc-thread-empty]')).toHaveCount(0);
  // A missing error notice alone can pass while the first-message read is still unresolved.
  await expect.poll(async () => {
    const reads = requests.filter(pending => pending.method() === 'GET' && pending.url() === runUrl);
    const response = await reads.at(-1)?.response();
    if (response?.status() !== 200) return false;
    const run = await response.json() as { input_contract: string; phase: string };
    return run.input_contract === 'canonical' && ['turn_running', 'turn_completed'].includes(run.phase);
  }, { message: 'the browser must reach a successful current run read after activation' }).toBe(true);
  await expect(drawer.getByText('Checking the first message’s current status.', { exact: true })).toHaveCount(0);
  await expect(drawer.getByText('The first message’s current status could not be checked.', { exact: true })).toHaveCount(0);
  const identityResponse = await request.get('/api/auth/whoami');
  expect(identityResponse.ok()).toBe(true);
  const identity = await identityResponse.json() as { userId: string; displayName: string };
  const items = await request.get(`/api/cards/${conversation.id}/harness/items?contract=conversation_v1`);
  expect(items.ok()).toBe(true);
  const conversationPage = await items.json() as ConversationPage;
  expect(conversationPage).toMatchObject({ contract: 'conversation_v1', source: 'presentation' });
  const humans = conversationPage.items.filter(item => item.source.kind === 'human');
  expect(humans).toHaveLength(1);
  expect(humans[0].source).toMatchObject({ kind: 'human', message_id: expect.stringMatching(/\S/), revision: 1,
    author: { user_id: identity.userId, display_name: identity.displayName }, text: message, attachments: [] });

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
  expect(await errors()).toEqual([]);
});

/* The endpoint's own guards, straight over HTTP; these do not depend on codex. */
test('the track conversations endpoint refuses a create it cannot make retryable', async ({ request }) => {
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  const track = await createTrack(request, area.id, `FE e2e conversation guards ${Date.now()}`);

  const noKey = await request.post(`/api/tracks/${track.id}/conversations`, { data: { provider: 'codex', text: 'hello' } });
  expect(noKey.status(), await noKey.text()).toBe(400);

  const blank = await request.post(`/api/tracks/${track.id}/conversations`, {
    headers: { 'Idempotency-Key': crypto.randomUUID() },
    data: { provider: 'codex', text: '   ' },
  });
  expect(blank.status(), await blank.text()).toBe(400);

  const missing = await request.post('/api/tracks/00000000000000000000000000000000/conversations', {
    headers: { 'Idempotency-Key': crypto.randomUUID() },
    data: { provider: 'codex', text: 'hello' },
  });
  expect(missing.status(), await missing.text()).toBe(404);
});

test('archives and restores the original Assistant through real HTTP and query invalidation', async ({ page, request }) => {
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  const track = await createTrack(request, area.id, `Archive API e2e ${Date.now()}`);
  const message = 'fake-reply: archived answer';
  const created = await request.post(`/api/tracks/${track.id}/conversations`, {
    headers: { 'idempotency-key': `archive-${Date.now()}` },
    data: { provider: 'codex', text: message },
  });
  expect(created.status(), await created.text()).toBe(201);
  const row = await created.json() as { id: string };
  const runPath = `/api/cards/${row.id}/planner/run?contract=conversation_v1`;
  await expect.poll(async () => (await (await request.get(runPath)).json() as { phase: string }).phase)
    .toBe('turn_completed');
  const before = await (await request.get(runPath)).json() as { worker_session_id: string };
  const historyPath = `/api/cards/${row.id}/harness/items?contract=conversation_v1`;
  const historyResponse = await request.get(historyPath);
  expect(historyResponse.ok()).toBe(true);
  const history = await historyResponse.json() as ConversationPage;
  expect(history).toMatchObject({ contract: 'conversation_v1', source: 'presentation' });
  expect(history.items.length).toBeGreaterThan(0);
  await page.goto(`/next/track/${track.id}`);
  const actions = page.getByRole('button', { name: `Actions for conversation: ${message}`, exact: true });
  await actions.click();
  const [archived] = await Promise.all([
    page.waitForResponse(response => response.request().method() === 'PATCH' && response.url().endsWith(`/${row.id}/archive`)),
    page.getByRole('menuitem', { name: 'Archive conversation', exact: true }).click(),
  ]);
  expect(archived.status(), await archived.text()).toBe(204);
  const listed = await (await request.get(`/api/tracks/${track.id}/conversations`)).json() as { id: string; title: string | null; archivedAt: number | null }[];
  // The same server row is named from its first input, rather than the default Assistant label.
  expect(listed.find(item => item.id === row.id)?.title).toBe(message);
  expect(listed.find(item => item.id === row.id)?.archivedAt).toEqual(expect.any(Number));
  await page.getByRole('button', { name: 'Archived conversations', exact: true }).click();
  await page.getByRole('button', { name: `Conversation ${message}`, exact: true }).click();
  await expect(page.getByText('This conversation is archived. Restore it to continue.')).toBeVisible();
  await expect(page.getByRole('combobox', { name: 'Message' })).toHaveCount(0);
  await expect(page.getByText('archived answer', { exact: true })).toBeVisible();
  expect(await (await request.get(historyPath)).json()).toEqual(history);
  const refused = await request.post(`/api/cards/${row.id}/planner/input`, {
    headers: { 'idempotency-key': `archived-refusal-${Date.now()}` }, data: { text: 'must not issue' },
  });
  expect(refused.status(), await refused.text()).toBe(409);
  await page.reload();
  await expect(page.getByText('This conversation is archived. Restore it to continue.')).toBeVisible();
  const [restored] = await Promise.all([
    page.waitForResponse(response => response.request().method() === 'PATCH' && response.url().endsWith(`/${row.id}/archive`)),
    page.getByRole('button', { name: 'Restore conversation', exact: true }).click(),
  ]);
  expect(restored.status(), await restored.text()).toBe(204);
  await expect(page.getByRole('combobox', { name: 'Message' })).toBeVisible();
  const after = await (await request.get(runPath)).json() as { worker_session_id: string };
  expect(after.worker_session_id).toBe(before.worker_session_id);
  expect(await (await request.get(historyPath)).json()).toEqual(history);
});
