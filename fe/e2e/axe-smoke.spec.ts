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

  for (const path of ['/', `/cove/${cove.id}`, `/wave/${wave.id}`, '/settings']) {
    await page.goto(path);
    await expect(page.locator('nav[aria-label="Workspace"]')).toBeVisible();
    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
      .analyze();
    expect(results.violations, `${path}: ${results.violations.map((item) => item.id).join(', ')}`).toEqual([]);
  }
});
