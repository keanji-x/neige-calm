// E2E: clicking "+ Add → New terminal" must show the card immediately,
// without a manual refresh.
//
// User-reported regression check: after clicking "+ Add → New terminal"
// on a wave, the card should render within a few seconds (POST →
// server card.added event → eventBridge invalidates ['wave', id] →
// useWaveDetailQuery refetches → WaveGrid mounts the new card). This
// spec pins that contract so a regression has a deterministic repro.
//
// Prereq: `make dev` serving http://localhost:4040 with the default seed.

import { test, expect } from '@playwright/test';

test('newly created terminal card appears without a reload', async ({ page }) => {
  // Step 1 — open Scratch cove and create a fresh wave to test in.
  await page.goto('/calm/');
  const scratch = page.getByRole('button', { name: /scratch/i });
  await expect(scratch).toBeVisible();
  await scratch.click();
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

  const newWaveBtn = page.getByRole('button', { name: /new wave/i });
  await newWaveBtn.click();
  const waveTitle = `E2E new-terminal ${Date.now()}`;
  const titleInput = page.getByPlaceholder(/wave title/i);
  await titleInput.fill(waveTitle);
  await titleInput.press('Enter');

  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/);
  await expect(page.getByText(waveTitle, { exact: false }).first()).toBeVisible();

  // Step 2 — the wave starts empty.
  await expect(page.locator('.term')).toHaveCount(0);

  // Step 3 — open the AddPanel and choose "New terminal". The AddPanel
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

  // Step 4 — the card must render WITHOUT a manual reload. Generous
  // timeout for slow CI; healthy local runs are sub-second. `.term` is
  // the class on the rendered terminal card — see
  // `web/src/cards/builtins/terminal.tsx` (`<div className={'term' …`).
  await expect(page.locator('.term')).toHaveCount(1, { timeout: 10_000 });
});
