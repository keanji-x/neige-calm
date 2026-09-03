import { AxeBuilder } from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import { createCove, createWave } from './helpers/seed.js';

const createdCoveIds: string[] = [];

test.beforeEach(() => { createdCoveIds.length = 0; });
test.afterEach(async ({ request }) => {
  for (const id of createdCoveIds) await request.delete(`/api/coves/${id}`);
  createdCoveIds.length = 0;
});

test('the primary routes have no WCAG A or AA violations in light mode', async ({ page, request }) => {
  const cove = await createCove(request);
  createdCoveIds.push(cove.id);
  const wave = await createWave(request, cove.id);

  const routes = [
    { path: '/next/', anchor: page.locator('section[aria-label="Today terminal"]') },
    { path: `/next/cove/${cove.id}`, anchor: page.locator('[data-nc-page-title]', { hasText: cove.name }) },
    /* #1211 — the new-wave page. Added here rather than left to the create
       spec: this is a real route with its own heading, a `contenteditable` and
       two chips, and none of that is exercised for contrast or naming by a spec
       that only drives it. It is anchored on the composer because the page has
       no `data-nc-page-title` — deliberately, the greeting is its one title. */
    { path: `/next/cove/${cove.id}/new`, anchor: page.getByLabel('What this wave should do') },
    { path: `/next/wave/${wave.id}`, anchor: page.locator('[data-nc-page-title]', { hasText: wave.title }) },
    { path: '/next/settings', anchor: page.getByRole('textbox', { name: 'HTTP proxy' }) },
  ];

  for (const route of routes) {
    await page.goto(route.path);
    await expect(page.locator('nav[aria-label="Workspace"]')).toBeVisible();
    await expect(route.anchor).toBeVisible();
    /*
     * Colour is only measurable once the page has stopped moving.
     *
     * Settings renders inside `ui/dialog`, whose panel fades in over the scrim
     * (`dialog-enter`). Playwright calls the panel visible as soon as it has a
     * box — opacity is not part of that verdict — so axe could sample a
     * *blend* of the panel and the scrim behind it and report a contrast
     * violation that no reader ever sees: measured, secondary text on a
     * half-faded panel came out at 3.27:1 against a background (#e3e2e0) the
     * settled page never paints. Waiting for the animations to finish makes
     * the sample the state the reader is actually in, and is why this used to
     * fail on the first attempt and pass on the retry.
     */
    await page.evaluate(() => Promise.all(
      document.getAnimations().map((animation) => animation.finished.catch(() => undefined)),
    ));
    const results = await new AxeBuilder({ page })
      .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
      .analyze();
    expect(results.violations, `${route.path}: ${results.violations.map((item) => item.id).join(', ')}`).toEqual([]);
  }
});
