// E2E: create a wave end-to-end and land on its detail page.
//
// Extends `golden-path.spec.ts` (sidebar → cove navigation) one step
// further: inside the Scratch cove, click the "+ New wave" compose bar,
// type a title, press Enter, then assert the URL transitions to
// `/calm/wave/<id>` and the wave page renders.
//
// Prereq (same as golden-path): `make dev` must be serving the docker
// stack at http://localhost:4040 with the default seed. We use a unique
// title per run (`E2E wave <timestamp>`) so re-runs don't collide with
// existing waves left over from prior failed runs.

import { test, expect } from '@playwright/test';

test('creates a new wave from the cove page and navigates to it', async ({ page }) => {
  await page.goto('/calm/');

  // Step 1 — sidebar → Scratch cove. Same anchor logic as golden-path.
  const scratch = page.locator('button.cove-nav', { hasText: 'Scratch' });
  await expect(scratch).toBeVisible();
  await scratch.click();
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

  // Step 2 — open the "+ New wave" inline compose bar. The CTA is the
  // last `.add-panel` button on the cove page (the same class is reused
  // by WavePage's `+ Add panel`, but here it's the only one on screen).
  const newWaveBtn = page.getByRole('button', { name: /new wave/i });
  await expect(newWaveBtn).toBeVisible();
  await newWaveBtn.click();

  // Step 3 — the button morphs into a single-line text input with
  // placeholder "Wave title…". Type a unique title and press Enter.
  // (Avoid blur — blur also submits, but Enter is the documented path.)
  const title = `E2E wave ${Date.now()}`;
  const input = page.getByPlaceholder(/wave title/i);
  await expect(input).toBeVisible();
  await input.fill(title);
  await input.press('Enter');

  // Step 4 — URL transitions to /calm/wave/<id> and the wave page mounts.
  // We allow up to ~5s for the round-trip (kernel insert + WS fanout +
  // router push) — playwright's default is plenty under normal load.
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/);

  // The wave title we just submitted should appear on the page; this is
  // the cheapest "the wave really rendered" assertion.
  await expect(page.getByText(title, { exact: false }).first()).toBeVisible();
});
