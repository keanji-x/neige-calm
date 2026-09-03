// E2E: clicking "Add card → terminal" must show the card immediately,
// without a manual refresh.
//
// User-reported regression check: after clicking "Add card → terminal"
// on a track, the card should render within a few seconds (POST →
// server card.added event → eventBridge invalidates ['track', id] →
// useTrackDetailQuery refetches → TrackGrid mounts the new card). This
// test pins that contract so a regression has a deterministic repro.
//
// Issue #175 — the kernel hides the system area that hosts the default
// Today terminal, so we mint our own user area + track to test in.
//
// Prereq: `make dev` serving http://localhost:4041 with the default seed.

import { test, expect } from '@playwright/test';

test('newly created terminal card appears without a reload', async ({ page }) => {
  // Step 1 — mint a fresh user area via the sidebar (issue #175).
  await page.goto('/calm/');
  const sidebarAreas = page.getByRole('navigation', { name: 'Areas' });
  const areaName = `E2E area ${Date.now()}`;
  await sidebarAreas.getByRole('button', { name: /new area/i }).click();
  const nameInput = sidebarAreas.getByPlaceholder(/name/i);
  await expect(nameInput).toBeVisible();
  await nameInput.fill(areaName);
  await nameInput.press('Enter');

  // `exact: true` excludes the per-row "Delete area \"<name>\"" button
  // whose accessible name also contains areaName — without exact match
  // the locator hits both and trips Playwright's strict mode.
  const areaBtn = sidebarAreas.getByRole('button', { name: areaName, exact: true });
  await expect(areaBtn).toBeVisible();
  await areaBtn.click();
  await expect(page).toHaveURL(/\/calm\/area\/[^/]+$/);

  // Step 2 — create a new track inside this area via the kernel REST
  // API directly. PR 3's NewTaskForm wires the area-page "+ New track"
  // CTA to the same shared flow, but for this test (which is purely
  // about the AddPanel terminal-card path inside an existing track)
  // the REST-direct route is faster and decouples this assertion from
  // the form's UI evolution. `page.request` resolves the relative URL
  // against this project's baseURL (set in playwright.config.ts →
  // 'chromium': http://localhost:4041/calm/). The helpers/reset.ts
  // variant is replay-port-pinned and only safe for the a11y project.
  const areaId = new URL(page.url()).pathname.split('/').pop()!;
  const trackTitle = `E2E new-terminal ${Date.now()}`;
  const trackRes = await page.request.post('/api/tracks', {
    data: {
      area_id: areaId,
      title: trackTitle,
      // #1147 S3 — no `cwd`: take the kernel-managed workspace branch.
      // This test is about the AddPanel terminal-card path, not working
      // directories. See `helpers/reset.ts::createTrackInArea` for why
      // the invented `/tmp/playwright-area-<id>` attached path was
      // never valid.
      // #177 — `theme` is a required NewTrack field. Mirrors
      // `DARK_THEME_RGB` in web/src/api/themeRgb.ts.
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!trackRes.ok()) {
    const body = await trackRes.text().catch(() => '<unreadable>');
    throw new Error(`POST /api/tracks → ${trackRes.status()} ${trackRes.statusText()}: ${body}`);
  }
  const track = (await trackRes.json()) as { id: string };
  await page.goto(`/calm/track/${track.id}`);
  await expect(page).toHaveURL(/\/calm\/track\/[^/]+$/);
  await expect(page.getByText(trackTitle, { exact: false }).first()).toBeVisible();

  // Step 3 — the track starts empty.
  await expect(page.locator('.term')).toHaveCount(0);

  // Step 4 — open the AddPanel and choose "terminal". The AddPanel
  // trigger is a glyph-only button since #594 (a `+` that rotates to
  // `×` while open); its accessible name is the aria-label "Add card"
  // while closed — see `web/src/shared/components/AddPanel.tsx`. The
  // "terminal" menu entry is a `role="menuitem"` button populated from
  // the cards registry (`web/src/cards/builtins/terminal.tsx` →
  // `addPanel: { label: 'terminal' }`). The menuitem renders a
  // card-head-style letter-avatar (aria-hidden) + the uppercase label;
  // the accessible name stays the lowercase kind word "terminal".
  const addBtn = page.getByRole('button', { name: /add card/i }).first();
  await expect(addBtn).toBeVisible();
  await addBtn.click();

  const termOption = page.getByRole('menuitem', { name: /terminal/i });
  await expect(termOption).toBeVisible({ timeout: 5_000 });
  await termOption.click();

  // Step 5 — the card must render WITHOUT a manual reload. Generous
  // timeout for slow CI; healthy local runs are sub-second. `.term` is
  // the class on the rendered terminal card — see
  // `web/src/cards/builtins/terminal.tsx` (`<div className={'term' …`).
  await expect(page.locator('.term')).toHaveCount(1, { timeout: 10_000 });
});
