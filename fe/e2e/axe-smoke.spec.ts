import { AxeBuilder } from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import { createCove, createWave } from './helpers/seed.js';

const createdCoveIds: string[] = [];

test.beforeEach(() => { createdCoveIds.length = 0; });
test.afterEach(async ({ request }) => {
  for (const id of createdCoveIds) await request.delete(`/api/coves/${id}`);
  createdCoveIds.length = 0;
});

test('the four primary routes have no WCAG A or AA violations in light mode', async ({ page, request }) => {
  const cove = await createCove(request);
  createdCoveIds.push(cove.id);
  const wave = await createWave(request, cove.id);

  const routes = [
    { path: '/next/', anchor: page.locator('section[aria-label="Today terminal"]') },
    { path: `/next/cove/${cove.id}`, anchor: page.locator('[data-nc-page-title]', { hasText: cove.name }) },
    { path: `/next/wave/${wave.id}`, anchor: page.locator('[data-nc-page-title]', { hasText: wave.title }) },
    { path: '/next/settings', anchor: page.getByRole('radiogroup', { name: 'Appearance' }) },
  ];

  for (const route of routes) {
    await page.goto(route.path);
    await expect(page.locator('nav[aria-label="Workspace"]')).toBeVisible();
    await expect(route.anchor).toBeVisible();
    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
      .analyze();
    expect(results.violations, `${route.path}: ${results.violations.map((item) => item.id).join(', ')}`).toEqual([]);
  }
});
