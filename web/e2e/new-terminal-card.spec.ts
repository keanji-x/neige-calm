// E2E: clicking "+ Add → New terminal" must show the card immediately,
// without a manual refresh.
//
// User-reported regression check: after clicking "+ Add → New terminal"
// on a wave, the card should render within a few seconds (POST →
// server card.added event → eventBridge invalidates ['wave', id] →
// useWaveDetailQuery refetches → WaveGrid mounts the new card). This
// spec pins that contract so a regression has a deterministic repro.
//
// Issue #175 — the kernel hides the system cove that hosts the default
// Today terminal, so we mint our own user cove + wave to test in.
//
// Prereq: `make dev` serving http://localhost:4040 with the default seed.

import { test, expect } from '@playwright/test';
import { createWaveInCove } from './helpers/reset';

test('newly created terminal card appears without a reload', async ({ page }) => {
  // Step 1 — mint a fresh user cove via the sidebar (issue #175).
  await page.goto('/calm/');
  const sidebarCoves = page.getByRole('navigation', { name: 'Coves' });
  const coveName = `E2E cove ${Date.now()}`;
  await sidebarCoves.getByRole('button', { name: /new cove/i }).click();
  const nameInput = sidebarCoves.getByPlaceholder(/name/i);
  await expect(nameInput).toBeVisible();
  await nameInput.fill(coveName);
  await nameInput.press('Enter');

  const coveBtn = sidebarCoves.getByRole('button', { name: new RegExp(coveName, 'i') });
  await expect(coveBtn).toBeVisible();
  await coveBtn.click();
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

  // Step 2 — create a new wave inside this cove. We use the API helper
  // because the cove-page "+ New wave" CTA is disabled in #250 PR 2
  // pending PR 3's NewTaskForm — this spec is about terminal-card
  // mounting, not the wave-create UX, so the setup path is incidental.
  const coveId = new URL(page.url()).pathname.split('/').pop()!;
  const waveTitle = `E2E new-terminal ${Date.now()}`;
  const wave = await createWaveInCove(page.request, coveId, waveTitle);
  await page.goto(`/calm/wave/${wave.id}`);
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/);
  await expect(page.getByText(waveTitle, { exact: false }).first()).toBeVisible();

  // Step 3 — the wave starts empty.
  await expect(page.locator('.term')).toHaveCount(0);

  // Step 4 — open the AddPanel and choose "New terminal". The AddPanel
  // trigger renders as a button with visible text "+ Add" (title="Add
  // card") — see `web/src/shared/components/AddPanel.tsx`. The "New
  // terminal" menu entry is a `role="menuitem"` button populated from
  // the cards registry (`web/src/cards/builtins/terminal.tsx` →
  // `addPanel: { label: 'New terminal' }`).
  const addBtn = page.getByRole('button', { name: /^\s*\+?\s*add(\s|$)/i }).first();
  await expect(addBtn).toBeVisible();
  await addBtn.click();

  const termOption = page.getByRole('menuitem', { name: /new terminal/i });
  await expect(termOption).toBeVisible({ timeout: 5_000 });
  await termOption.click();

  // Step 5 — the card must render WITHOUT a manual reload. Generous
  // timeout for slow CI; healthy local runs are sub-second. `.term` is
  // the class on the rendered terminal card — see
  // `web/src/cards/builtins/terminal.tsx` (`<div className={'term' …`).
  await expect(page.locator('.term')).toHaveCount(1, { timeout: 10_000 });
});
