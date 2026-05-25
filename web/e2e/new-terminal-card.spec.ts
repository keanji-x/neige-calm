// E2E: clicking "+ Add → terminal" must show the card immediately,
// without a manual refresh.
//
// User-reported regression check: after clicking "+ Add → terminal"
// on a wave, the card should render within a few seconds (POST →
// server card.added event → eventBridge invalidates ['wave', id] →
// useWaveDetailQuery refetches → WaveGrid mounts the new card). This
// spec pins that contract so a regression has a deterministic repro.
//
// Issue #175 — the kernel hides the system cove that hosts the default
// Today terminal, so we mint our own user cove + wave to test in.
//
// Prereq: `make dev` serving http://localhost:4041 with the default seed.

import { test, expect } from '@playwright/test';

test('newly created terminal card appears without a reload', async ({ page }) => {
  // Block Google Fonts. `index.html` loads a `<link rel="stylesheet"
  // href="https://fonts.googleapis.com/...">` that, in restricted-network
  // CI / sandboxed test environments, can hang for tens of seconds
  // before failing — blocking `domcontentloaded` and module-script
  // execution. The fallback `system-ui` chain is good enough for the
  // assertions below.
  await page.route('**://fonts.googleapis.com/**', (route) => route.abort());
  await page.route('**://fonts.gstatic.com/**', (route) => route.abort());

  // Step 1 — mint a fresh user cove via the sidebar (issue #175).
  await page.goto('/calm/');
  const sidebarCoves = page.getByRole('navigation', { name: 'Coves' });
  const coveName = `E2E cove ${Date.now()}`;
  await sidebarCoves.getByRole('button', { name: /new cove/i }).click();
  const nameInput = sidebarCoves.getByPlaceholder(/name/i);
  await expect(nameInput).toBeVisible();
  await nameInput.fill(coveName);
  await nameInput.press('Enter');

  // `exact: true` excludes the per-row "Delete cove \"<name>\"" button
  // whose accessible name also contains coveName — without exact match
  // the locator hits both and trips Playwright's strict mode.
  const coveBtn = sidebarCoves.getByRole('button', { name: coveName, exact: true });
  await expect(coveBtn).toBeVisible();
  await coveBtn.click();
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

  // Step 2 — create a new wave inside this cove via the kernel REST
  // API directly. PR 3's NewTaskForm wires the cove-page "+ New wave"
  // CTA to the same shared flow, but for this spec (which is purely
  // about the AddPanel terminal-card path inside an existing wave)
  // the REST-direct route is faster and decouples this assertion from
  // the form's UI evolution. `page.request` resolves the relative URL
  // against this project's baseURL (set in playwright.config.ts →
  // 'chromium': http://localhost:4041/calm/). The helpers/reset.ts
  // variant is replay-port-pinned and only safe for the a11y project.
  const coveId = new URL(page.url()).pathname.split('/').pop()!;
  const waveTitle = `E2E new-terminal ${Date.now()}`;
  const cwd = `/tmp/playwright-cove-${coveId}`;
  const waveRes = await page.request.post('/api/waves', {
    data: {
      cove_id: coveId,
      title: waveTitle,
      cwd,
      attach_folder: true,
      // #177 — `theme` is a required NewWave field. Mirrors
      // `DARK_THEME_RGB` in web/src/api/themeRgb.ts.
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!waveRes.ok()) {
    const body = await waveRes.text().catch(() => '<unreadable>');
    throw new Error(`POST /api/waves → ${waveRes.status()} ${waveRes.statusText()}: ${body}`);
  }
  const wave = (await waveRes.json()) as { id: string };
  await page.goto(`/calm/wave/${wave.id}`);
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/);
  await expect(page.getByText(waveTitle, { exact: false }).first()).toBeVisible();

  // Step 3 — the wave starts empty.
  await expect(page.locator('.term')).toHaveCount(0);

  // Step 4 — open the AddPanel and choose "terminal". The AddPanel
  // trigger renders as a button with visible text "+ Add" (title="Add
  // card") — see `web/src/shared/components/AddPanel.tsx`. The
  // "terminal" menu entry is a `role="menuitem"` button populated from
  // the cards registry (`web/src/cards/builtins/terminal.tsx` →
  // `addPanel: { label: 'terminal' }`). The menuitem renders a
  // card-head-style letter-avatar (aria-hidden) + the uppercase label;
  // the accessible name stays the lowercase kind word "terminal".
  const addBtn = page.getByRole('button', { name: /^\s*\+?\s*add(\s|$)/i }).first();
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
