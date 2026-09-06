import { expect, test } from '@playwright/test';

test('keeps Today usable without claiming zero activity when Areas is unavailable', async ({ page }) => {
  // Recovery starts with the real Retry click. The flag lives in Node rather
  // than on the page: the handler then decides synchronously, so serving the
  // stub never waits on a page round-trip, and flipping it immediately before
  // the click still happens before the request that click triggers.
  let recovered = false;
  let served = 0;
  await page.route('**/api/areas', async (route) => {
    if (!recovered && route.request().method() === 'GET') {
      await route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'Areas temporarily unavailable' }) });
      served += 1;
    } else await route.continue();
  });
  await page.goto('/next/');
  const main = page.getByRole('main');
  const failure = main.getByRole('alert').filter({ hasText: 'Areas temporarily unavailable' });
  // #1529 — the state under test cannot exist before the stub has answered.
  // Waiting for that answer in Node first means a slow page load is reported as
  // "the stub was never asked", not as "the failure state never rendered".
  await expect.poll(() => served, { timeout: 15_000 }).toBeGreaterThan(0);
  // The alert is the state under test, not a deadline: how long the app takes to
  // surface a failed read (a retry policy may sit in front of it) is not what
  // this test asserts, so the wait is explicit and generous. Everything after it
  // runs against the settled failure state on the default budget.
  await expect(failure).toBeVisible({ timeout: 15_000 });
  const header = main.locator('header[data-nc-header-rows]').first();
  // #1529 — existence first. `not.toContainText` on its own reports `element(s)
  // not found` for a header that never rendered, which is the same message as a
  // header that rendered a count: two opposite verdicts under one failure.
  await expect(header).toBeVisible();
  await expect(header).not.toContainText(/\d\s*(waiting|running)/);
  await expect(main.getByText('Nothing scheduled.')).toHaveCount(0);
  await expect(page.getByRole('navigation', { name: 'Workspace' })).toBeVisible();
  await page.screenshot({ path: 'test-results/today-unavailable-desktop.png' });
  const retry = failure.getByRole('button', { name: 'Retry' });
  recovered = true;
  await retry.click();
  await expect(failure).toHaveCount(0);
  await expect(header).toContainText(/\d\s*running/);
});

test('retains the original Track request after a lost acknowledgement and navigation', async ({ page, request, context }) => {
  const response = await request.post('/api/areas', { data: { name: `Track recovery ${Date.now()}`, color: '#123456' } });
  expect(response.ok()).toBe(true);
  const area = await response.json() as { id: string; name: string };
  const attempts: { key: string | undefined; body: unknown }[] = [];
  let createdId: string | undefined;
  try {
    await page.route('**/api/tracks', async (route) => {
      if (route.request().method() !== 'POST') { await route.continue(); return; }
      attempts.push({ key: route.request().headers()['idempotency-key'], body: route.request().postDataJSON() as unknown });
      if (attempts.length > 1) { await route.continue(); return; }
      const created = await route.fetch();
      expect(created.status()).toBe(201);
      createdId = (await created.json() as { id: string }).id;
      await route.abort('failed');
    });
    await page.goto(`/next/area/${area.id}/new`);
    const composer = page.getByRole('textbox', { name: 'What this track should do' });
    await composer.fill('Keep exactly one Track for this intention.');
    await page.getByRole('button', { name: 'Create track', exact: true }).click();
    await expect(page.getByRole('alert').filter({ hasText: 'Transport request failed' })).toBeVisible();
    await context.setOffline(true);
    await page.getByRole('button', { name: 'Create track', exact: true }).click();
    await expect(page.getByRole('alert').filter({ hasText: 'original creation is still unconfirmed' })).toBeVisible();
    await expect(composer).toHaveAttribute('contenteditable', 'false');
    expect(attempts).toHaveLength(1);
    await context.setOffline(false);
    await page.getByRole('button', { name: 'Go to Today' }).click();
    await page.getByRole('button', { name: `New track in ${area.name}` }).click();
    await expect(composer).toHaveText('Keep exactly one Track for this intention.');
    await page.getByRole('button', { name: 'Create track', exact: true }).click();
    await expect(page).toHaveURL(new RegExp(`/next/track/${createdId}`));
    expect(attempts).toHaveLength(2);
    expect(attempts[1]).toEqual(attempts[0]);
    const tracks = await (await request.get(`/api/areas/${area.id}/tracks`)).json() as { id: string }[];
    expect(tracks.map((track) => track.id)).toEqual([createdId]);
  } finally {
    await context.setOffline(false);
    await request.delete(`/api/areas/${area.id}`);
  }
});

test('keeps a deleted Area draft selectable and never creates an orphan Track', async ({ page, request }) => {
  const area = await (await request.post('/api/areas', { data: { name: `Deleted draft ${Date.now()}`, color: '#123456' } })).json() as { id: string };
  const creates: string[] = [];
  page.on('request', (entry) => { if (entry.method() === 'POST' && entry.url().endsWith('/api/tracks')) creates.push(entry.url()); });
  try {
    await page.goto(`/next/area/${area.id}/new`);
    const composer = page.getByRole('textbox', { name: 'What this track should do' });
    await composer.fill('Keep this unfinished draft after its parent is deleted.');
    expect((await request.delete(`/api/areas/${area.id}`)).ok()).toBe(true);
    await expect(page.getByRole('alert').filter({ hasText: 'Your draft is kept here' })).toBeVisible();
    await expect(composer).toHaveText('Keep this unfinished draft after its parent is deleted.');
    await expect(page.getByRole('button', { name: 'Create track', exact: true })).toBeDisabled();
    await page.getByRole('button', { name: 'Select draft' }).click();
    expect(await page.evaluate(() => window.getSelection()?.toString())).toBe('Keep this unfinished draft after its parent is deleted.');
    await composer.press('Enter');
    expect(creates).toEqual([]);
    await page.screenshot({ path: 'test-results/deleted-area-draft-desktop.png' });
  } finally {
    await request.delete(`/api/areas/${area.id}`);
  }
});

test('does not replay an offline settings edit over another client and refreshes the other pane', async ({ page, context, browser, request, baseURL }) => {
  const original = await (await request.get('/api/settings')).json() as { settings: Record<string, string> };
  const otherContext = await browser.newContext({ baseURL });
  const other = await otherContext.newPage();
  try {
    await request.put('/api/settings', { data: { settings: { task_budget_default: '1' } } });
    await page.goto('/next/settings');
    await other.goto('/next/settings');
    const mine = page.getByLabel('Task concurrency');
    await expect(mine).toHaveValue('1');
    await context.setOffline(true);
    await mine.fill('2'); await mine.press('Tab');
    await expect(page.getByText(/offline.*Reconnect/i)).toBeVisible();
    const theirs = other.getByLabel('Task concurrency');
    await theirs.fill('3'); await theirs.press('Tab');
    await expect.poll(async () => (await (await request.get('/api/settings')).json() as { settings: Record<string, string> }).settings.task_budget_default).toBe('3');
    await context.setOffline(false);
    await expect(page.getByText('Changed elsewhere to 3. Your edit is not saved.')).toBeVisible();
    await expect(mine).toHaveValue('2');
    expect((await (await request.get('/api/settings')).json() as { settings: Record<string, string> }).settings.task_budget_default).toBe('3');
    await request.put('/api/settings', { data: { settings: { task_budget_default: '4' } } });
    await expect(theirs).toHaveValue('4', { timeout: 20_000 });
  } finally {
    await context.setOffline(false);
    await otherContext.close();
    await page.close();
    await request.put('/api/settings', { data: { settings: { task_budget_default: original.settings.task_budget_default ?? null } } });
  }
});
