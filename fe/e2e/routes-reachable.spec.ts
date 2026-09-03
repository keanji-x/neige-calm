import { expect, test, type Page } from '@playwright/test';
import { createCove, createWave } from './helpers/seed.js';

const createdCoveIds: string[] = [];

function captureBrowserErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
  page.on('pageerror', (error) => errors.push(error.message));
  return errors;
}

test.beforeEach(() => { createdCoveIds.length = 0; });
test.afterEach(async ({ request }) => {
  for (const id of createdCoveIds) await request.delete(`/api/coves/${id}`);
  createdCoveIds.length = 0;
});

test('the application routes are reachable through the real kernel', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const version = await request.get('/api/version');
  expect(version.ok()).toBe(true);
  expect(await version.json() as { dbInstanceId?: unknown }).toEqual(
    expect.objectContaining({ dbInstanceId: expect.stringMatching(/^[0-9a-f-]{36}$/i) }),
  );

  const cove = await createCove(request);
  createdCoveIds.push(cove.id);
  const wave = await createWave(request, cove.id);
  const routes = [
    { path: '/next/', anchor: page.locator('section[aria-label="Today terminal"]') },
    { path: `/next/cove/${cove.id}`, anchor: page.locator('[data-nc-page-title]', { hasText: cove.name }) },
    /* #1211 — the new-wave page is a route like the others, so it belongs in
       the reachability sweep: this is what would catch it failing to render at
       all behind the real kernel. Anchored on the composer because the page has
       no `data-nc-page-title` — deliberately, the greeting is its one title. */
    { path: `/next/cove/${cove.id}/new`, anchor: page.getByLabel('What this wave should do') },
    { path: `/next/wave/${wave.id}`, anchor: page.locator('[data-nc-page-title]', { hasText: wave.title }) },
    { path: '/next/settings', anchor: page.getByRole('textbox', { name: 'HTTP proxy' }) },
    { path: '/next/settings/appearance', anchor: page.getByRole('combobox', { name: 'Theme' }) },
  ];

  for (const route of routes) {
    await page.goto(route.path);
    await expect(page.locator('nav[aria-label="Workspace"]')).toBeVisible();
    await expect(route.anchor).toBeVisible();
  }
  expect(errors).toEqual([]);
});
