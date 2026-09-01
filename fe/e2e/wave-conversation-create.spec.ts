/*
 * #1189 S6 — starting a conversation from a wave page, against the real kernel.
 *
 * Before S5 the wave route forked on whether the wave had a spec card and
 * neither branch offered a `+`; there has never been a browser-level case for
 * creating a conversation at all. This is that case, and everything below is
 * served by the real server over the real HTTP surface — no mocked transport,
 * no fixture rows.
 *
 * ## What this job can and cannot reach
 *
 * `POST /api/waves/{id}/conversations` mints the card AND starts its codex
 * harness in one operation (`wave_conversations.rs`), so a 201 requires a live
 * shared codex app-server. The `fe e2e` job does not have one: `.github/
 * workflows/ci.yml` writes `CALM_CODEX_HOST_BIN=/bin/true` into `.env`, the
 * app-server exits before `initialize`, and the endpoint answers
 *
 *   500 {"code":"internal","error":"internal: shared codex app-server is not
 *        running (last error: … exited before initialize …); supervisor is
 *        retrying in the background — retry shortly"}
 *
 * — measured against a real `calm-server` run with `CALM_CODEX_BIN=/bin/true`,
 * in ~17ms, not a hang. So "the conversation appears in the list" is not
 * assertable here, and pretending otherwise would mean either a test that
 * always skips or one that asserts nothing.
 *
 * What IS assertable, and is what this file pins:
 *
 *   1. the wave page reads its conversations from the real endpoint and renders
 *      the empty list — the `'rows'` arm S5 replaced the spec-card fork with;
 *   2. the `+` is there, on an ordinary wave, and opens a draft;
 *   3. the first message produces **exactly one** POST, to the wave in the URL
 *      and to no other conversations endpoint, carrying the `Idempotency-Key`
 *      the retry contract is built on and the typed words as its body;
 *   4. the kernel treats that request as well-formed — it reaches the harness
 *      start rather than being turned away on shape or routing (a 400/403/404/
 *      405 would mean the browser is talking to the wrong endpoint, or in the
 *      wrong shape, which is exactly the class of bug an e2e is for);
 *   5. a create that fails says so in the drawer and offers a retry, and no
 *      conversation row is invented for it.
 *
 * Plus one block of pure-HTTP assertions on the endpoint's own guards, which
 * are deterministic in every environment.
 */

import { expect, test, type Page, type Request } from '@playwright/test';

import { createCove, createWave } from './helpers/seed.js';

const createdCoveIds: string[] = [];

function captureBrowserErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
  page.on('pageerror', (error) => errors.push(error.message));
  return errors;
}

/** Every POST the page made to any wave/cove conversations endpoint. */
function conversationCreates(requests: Request[]): Request[] {
  return requests.filter((request) => request.method() === 'POST'
    && /\/conversations$/.test(new URL(request.url()).pathname));
}

test.beforeEach(() => { createdCoveIds.length = 0; });
test.afterEach(async ({ request }) => {
  for (const id of createdCoveIds) await request.delete(`/api/coves/${id}`);
  createdCoveIds.length = 0;
});

test('starts a conversation from a wave page and sends the first message to that wave', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const cove = await createCove(request);
  createdCoveIds.push(cove.id);
  const wave = await createWave(request, cove.id, `FE e2e conversation wave ${Date.now()}`);

  const requests: Request[] = [];
  page.on('request', (pending) => requests.push(pending));

  await page.goto(`/next/wave/${wave.id}`);

  // (1) The list is the server's plus one row the route injects from the wave's
  // own spec card — which is the `'rows'` arm S5 replaced the spec-card fork
  // with, and it is visible here precisely because the two sources are
  // different: the endpoint lists assistant conversations only, and on a fresh
  // wave it lists none.
  const seeded = await request.get(`/api/waves/${wave.id}/conversations`);
  expect(seeded.ok()).toBe(true);
  expect(await seeded.json() as unknown[]).toEqual([]);
  await expect(page.getByRole('button', { name: 'Conversation Spec' })).toBeVisible();

  // (2) The `+`. On a wave, not a cove: this affordance did not exist here
  // before S5, and `source.kind === 'elsewhere'` still withholds it.
  await page.getByRole('button', { name: 'New conversation' }).click();
  await expect(page.getByRole('complementary', { name: 'Untitled' })).toBeVisible();

  // Opening the draft mints nothing — the card is born with the first message.
  expect(conversationCreates(requests)).toHaveLength(0);

  // (3) Type and send. The composer is a contenteditable that sends on Enter.
  const message = 'what does this wave do?';
  const composer = page.getByRole('combobox', { name: 'Message' });
  await composer.click();
  await composer.fill(message);
  const [created] = await Promise.all([
    page.waitForResponse((response) => response.request().method() === 'POST'
      && new URL(response.url()).pathname === `/api/waves/${wave.id}/conversations`),
    composer.press('Enter'),
  ]);

  const posts = conversationCreates(requests);
  expect(posts).toHaveLength(1);
  const post = posts[0];
  expect(new URL(post.url()).pathname).toBe(`/api/waves/${wave.id}/conversations`);
  expect(post.postDataJSON()).toEqual({ text: message });
  // The retry contract's whole basis: without this header the kernel 400s, and
  // a second attempt would mint a second conversation instead of retrying this
  // one.
  expect(await post.headerValue('idempotency-key')).toMatch(/[0-9a-f-]{36}/);

  // (4) The kernel accepted the shape and the route. 201 is what a stack with a
  // live codex answers; 500 is this job's app-server-less stack reaching the
  // harness start and failing there. Anything else means the browser asked the
  // wrong question.
  expect(
    [201, 500],
    `unexpected status ${created.status()}: ${await created.text()}`,
  ).toContain(created.status());

  if (created.status() === 201) {
    // A stack with codex: the answer is adopted and the row is listed.
    await expect(page.getByRole('complementary', { name: 'Assistant' })).toBeVisible();
    const listed = await request.get(`/api/waves/${wave.id}/conversations`);
    expect(listed.ok()).toBe(true);
    expect(await listed.json() as unknown[]).toHaveLength(1);
    expect(errors).toEqual([]);
  } else {
    // (5) This job's stack. The failure is *said*, with a retry offered, and
    // nothing is invented: an optimistic row here would be a conversation the
    // server has never heard of.
    expect(await created.json() as { error: string })
      .toHaveProperty('error', expect.stringContaining('shared codex app-server is not running'));
    await expect(page.getByRole('button', { name: 'Try again' })).toBeVisible();
    const listed = await request.get(`/api/waves/${wave.id}/conversations`);
    expect(listed.ok()).toBe(true);
    expect(await listed.json() as unknown[]).toEqual([]);
    // Chromium logs the refused POST itself; that one is the subject of this
    // branch, not a defect. Everything else still has to be silent — a crash
    // while rendering the failure is exactly what this would otherwise hide.
    expect(errors.filter((error) =>
      !/Failed to load resource: the server responded with a status of 500/.test(error))).toEqual([]);
  }
});

/*
 * The endpoint's own guards, straight over HTTP. These do not depend on codex,
 * so they are the same assertions in every environment — and they are what
 * makes the header and body the browser sends above load-bearing rather than
 * decorative.
 */
test('the wave conversations endpoint refuses a create it cannot make retryable', async ({ request }) => {
  const cove = await createCove(request);
  createdCoveIds.push(cove.id);
  const wave = await createWave(request, cove.id, `FE e2e conversation guards ${Date.now()}`);

  const noKey = await request.post(`/api/waves/${wave.id}/conversations`, { data: { text: 'hello' } });
  expect(noKey.status(), await noKey.text()).toBe(400);

  const blank = await request.post(`/api/waves/${wave.id}/conversations`, {
    headers: { 'Idempotency-Key': crypto.randomUUID() },
    data: { text: '   ' },
  });
  expect(blank.status(), await blank.text()).toBe(400);

  const missing = await request.post('/api/waves/00000000000000000000000000000000/conversations', {
    headers: { 'Idempotency-Key': crypto.randomUUID() },
    data: { text: 'hello' },
  });
  expect(missing.status(), await missing.text()).toBe(404);
});
