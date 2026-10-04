import { AxeBuilder } from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import { createArea, createTrack } from './helpers/seed.js';

const createdAreaIds: string[] = [];

test.beforeEach(() => { createdAreaIds.length = 0; });
test.afterEach(async ({ request }) => {
  for (const id of createdAreaIds) await request.delete(`/api/areas/${id}`);
  createdAreaIds.length = 0;
});

test('the primary routes have no WCAG A or AA violations in light mode', async ({ page, request }) => {
  const area = await createArea(request);
  createdAreaIds.push(area.id);
  const track = await createTrack(request, area.id);

  const routes = [
    /* The daily homepage uses the same report title as a Track page. */
    { path: '/next/', anchor: page.locator('[data-nc-page-title]').filter({ hasText: /\d{4}-\d{2}-\d{2}/ }) },
    /* Anchored on the composer: the new-track page has no `data-nc-page-title`; the greeting is its one title. */
    { path: `/next/area/${area.id}/new`, anchor: page.getByLabel('What this track should do') },
    { path: `/next/track/${track.id}`, anchor: page.locator('[data-nc-page-title]', { hasText: track.title }) },
    { path: '/next/settings', anchor: page.getByRole('textbox', { name: 'HTTP proxy' }) },
    { path: '/next/settings/network', anchor: page.getByRole('textbox', { name: 'HTTP proxy' }) },
  ];

  for (const route of routes) {
    await page.goto(route.path);
    await expect(page.locator('nav[aria-label="Workspace"]')).toBeVisible();
    await expect(route.anchor).toBeVisible();
    /* Wait for animations: Playwright calls a fading `ui/dialog` panel visible before it is opaque, so
     * axe could sample a blend of panel and scrim and report a contrast violation no reader sees. */
    await page.evaluate(() => Promise.all(
      document.getAnimations().map((animation) => animation.finished.catch(() => undefined)),
    ));
    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
      .analyze();
    expect(results.violations, `${route.path}: ${results.violations.map((item) => item.id).join(', ')}`).toEqual([]);
  }
});
