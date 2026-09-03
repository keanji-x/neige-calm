/*
 * #1189 S6 — starting a conversation from a track page, against the real kernel.
 *
 * Before S5 the track route forked on whether the track had a spec card and
 * neither branch offered a `+`; there has never been a browser-level case for
 * creating a conversation at all. This is that case, and everything below is
 * served by the real server over the real HTTP surface — no mocked transport,
 * no fixture rows.
 *
 * ## The create really succeeds here, and that is the point
 *
 * `POST /api/tracks/{id}/conversations` mints the card AND starts its codex
 * harness in one operation (`track_conversations.rs`), so a 201 requires a live
 * shared codex app-server. This spec previously accepted `201 | 500` because
 * the job had none — and that made it prove far less than it looked like it
 * did: the 500 is raised by the adapter's daemon preflight, which runs
 * *before* `prepare_tx` mints the card, the session and the MCP token. A
 * kernel that minted the card with the wrong role, without the
 * `harness_profile` marker, or with no token at all would have returned the
 * same 500 and this spec would still have been green. It pinned the browser's
 * request contract and nothing about the thing the request asks for.
 *
 * So the job now runs a codex app-server: `ci.yml` points
 * `CALM_CODEX_HOST_BIN` at the `osc-probe-child` fixture binary, which answers
 * `initialize` / `thread/start` / `turn/start` over the app-server socket
 * (`crates/calm-server/tests/fixtures/osc-probe-child/appserver.rs`) — the
 * same stand-in the Rust integration suites already use, and the reason they
 * can assert 201 on track create. 201 is therefore required, not tolerated.
 *
 * What this file pins:
 *
 *   1. the track page **reads** its conversations from the real endpoint — the
 *      `'rows'` arm S5 replaced the spec-card fork with — and renders the list
 *      that comes back;
 *   2. the `+` is there, on an ordinary track, and opens a draft;
 *   3. the first message produces **exactly one** POST, to the track in the URL
 *      and to no other conversations endpoint, carrying the `Idempotency-Key`
 *      the retry contract is built on and the typed words as its body — and
 *      still exactly one once the whole interaction has settled;
 *   4. the kernel mints a real assistant conversation for it: 201, and the
 *      conversation in that response is **the same card** the list endpoint
 *      then returns;
 *   5. the page shows that one conversation and no other — an optimistic row
 *      left beside the server's would be two rows for one card.
 *
 * The *failure* UX (the drawer says so, offers `Try again`, and invents no
 * row) is not here: forcing a failure against a healthy stack would mean
 * mocking the transport, which is the one thing this file exists not to do.
 * It is covered where a fake transport is honest —
 * `web/src/app/router/wave-conversation.test.tsx` (`[G5]`, and the
 * `Try again` cases around it).
 *
 * Plus one block of pure-HTTP assertions on the endpoint's own guards, which
 * are deterministic in every environment.
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

/** Every POST the page made to any track/area conversations endpoint. */
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

  // (1) The list on screen is the server's, plus one row the route injects
  // from the track's own spec card. Both halves are asserted, because either
  // one alone is satisfiable by a page that never asks the kernel anything:
  // that the page *made the request* (this array is the browser's own
  // traffic — `request.get` below is Playwright's, and never appears in it),
  // and that a fresh track's answer is empty while the spec row still shows.
  await expect(page.getByRole('button', { name: 'Conversation Spec' })).toBeVisible();
  expect(
    conversationReads(requests, track.id).length,
    'the page must read its conversations from GET /api/tracks/{id}/conversations',
  ).toBeGreaterThan(0);
  const seeded = await request.get(`/api/tracks/${track.id}/conversations`);
  expect(seeded.ok()).toBe(true);
  expect(await seeded.json() as unknown[]).toEqual([]);
  await expect(conversationRows(page)).toHaveCount(1);

  // (2) The `+`. On a track, not an area: this affordance did not exist here
  // before S5, and `source.kind === 'elsewhere'` still withholds it.
  await page.getByRole('button', { name: 'New conversation' }).click();
  await expect(page.getByRole('complementary', { name: 'Untitled' })).toBeVisible();

  // Opening the draft mints nothing — the card is born with the first message.
  expect(conversationCreates(requests)).toHaveLength(0);

  // (3) Type and send. The composer is a contenteditable that sends on Enter.
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
  // The retry contract's whole basis: without this header the kernel 400s, and
  // a second attempt would mint a second conversation instead of retrying this
  // one.
  expect(await post.headerValue('idempotency-key')).toMatch(/[0-9a-f-]{36}/);

  // (4) The kernel minted a conversation. Not `201 | 500`: with an app-server
  // present, anything but 201 means the browser asked the wrong question or
  // the mint itself failed, and both are exactly what this file is for.
  expect(created.status(), `create failed: ${await created.text()}`).toBe(201);
  const conversation = await created.json() as { id: string; trackId: string; kind: string };
  expect(conversation.trackId).toBe(track.id);
  expect(conversation.kind).toBe('track-assistant');

  // The card in the response and the card in the list are one card. This is
  // what the old `201 | 500` shape could not reach: it is the only assertion
  // here that fails if the mint writes a card the list predicate does not
  // match (wrong role, missing `harness_profile` marker, wrong track).
  const listed = await request.get(`/api/tracks/${track.id}/conversations`);
  expect(listed.ok()).toBe(true);
  expect(await listed.json() as { id: string }[]).toEqual([
    expect.objectContaining({ id: conversation.id, trackId: track.id }),
  ]);

  // (5) On screen: the conversation opened as a drawer, and behind it the
  // list now holds the spec row and this one conversation. An optimistic row
  // that was never reconciled with the server's would show up here as a third
  // — a conversation the user can see twice, or one the kernel never made.
  // (The drawer replaces the list while it is open, so the list is counted
  // after closing it — the count is the assertion, not the drawer.)
  await expect(page.getByRole('complementary', { name: 'Assistant' })).toBeVisible();
  await page.getByRole('button', { name: 'Close conversation' }).click();
  await expect(conversationRows(page)).toHaveCount(2);

  // A late duplicate POST — precisely the double-mint `Idempotency-Key`
  // exists to make harmless, and precisely what a retry-on-settle bug would
  // emit — lands after `waitForResponse` has already returned, so the count
  // above cannot see it. Give the interaction a bounded moment to finish
  // misbehaving, then count again.
  await page.waitForTimeout(1_000);
  expect(
    conversationCreates(requests),
    'the first message must mint once, and still once after the interaction settles',
  ).toHaveLength(1);
  expect(await (await request.get(`/api/tracks/${track.id}/conversations`)).json() as unknown[])
    .toHaveLength(1);
  expect(errors).toEqual([]);
});

/*
 * The endpoint's own guards, straight over HTTP. These do not depend on codex,
 * so they are the same assertions in every environment — and they are what
 * makes the header and body the browser sends above load-bearing rather than
 * decorative.
 */
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
