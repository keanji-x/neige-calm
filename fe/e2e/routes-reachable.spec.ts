import { expect, test, type Page } from '@playwright/test';
import { createArea, createTrack } from './helpers/seed.js';

const createdAreaIds: string[] = [];

function captureBrowserErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
  page.on('pageerror', (error) => errors.push(error.message));
  return errors;
}

test.beforeEach(() => { createdAreaIds.length = 0; });
test.afterEach(async ({ request }) => {
  for (const id of createdAreaIds) await request.delete(`/api/areas/${id}`);
  createdAreaIds.length = 0;
});

test('the application routes are reachable through the real kernel', async ({ page, request }) => {
  const errors = captureBrowserErrors(page);
  const version = await request.get('/api/version');
  expect(version.ok()).toBe(true);
  expect(await version.json() as { dbInstanceId?: unknown }).toEqual(
    expect.objectContaining({ dbInstanceId: expect.stringMatching(/^[0-9a-f-]{36}$/i) }),
  );

  const area = await createArea(request);
  createdAreaIds.push(area.id);
  const track = await createTrack(request, area.id);
  const routes = [
    /* Today's title is the date via `toLocaleDateString`, so a text match would depend on the
       browser's locale and the day; `Previous week` is the calendar's own control and exists on no other route. */
    { path: '/next/', anchor: page.getByRole('button', { name: 'Previous week' }) },
    /* Anchored on the composer because the new-track page has no `data-nc-page-title`. */
    { path: `/next/area/${area.id}/new`, anchor: page.getByLabel('What this track should do') },
    { path: `/next/track/${track.id}`, anchor: page.locator('[data-nc-page-title]', { hasText: track.title }) },
    { path: '/next/settings', anchor: page.getByRole('spinbutton', { name: 'Task concurrency' }) },
    { path: '/next/settings/network', anchor: page.getByRole('textbox', { name: 'HTTP proxy' }) },
    { path: '/next/settings/appearance', anchor: page.getByRole('combobox', { name: 'Theme' }) },
  ];

  for (const route of routes) {
    await page.goto(route.path);
    await expect(page.locator('nav[aria-label="Workspace"]')).toBeVisible();
    await expect(route.anchor).toBeVisible();
  }
  expect(errors).toEqual([]);
});
