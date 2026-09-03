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
    /* #1253 — see `routes-reachable`: Today's page title is a locale- and
       date-dependent string, so the anchor is the calendar module's week nav
       instead. It is Today-only, so a green run here means axe really scanned
       Today. */
    { path: '/next/', anchor: page.getByRole('button', { name: 'Previous week' }) },
    /* #1211 — the new-track page. Added here rather than left to the create
       test: this is a real route with its own heading, a `contenteditable` and
       two chips, and none of that is exercised for contrast or naming by a test
       that only drives it. It is anchored on the composer because the page has
       no `data-nc-page-title` — deliberately, the greeting is its one title. */
    { path: `/next/area/${area.id}/new`, anchor: page.getByLabel('What this track should do') },
    { path: `/next/track/${track.id}`, anchor: page.locator('[data-nc-page-title]', { hasText: track.title }) },
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
