// E2E: create a wave end-to-end and land on its detail page.
//
// After issue #175 there is no seeded `Scratch` cove in the sidebar.
// We mint our own user cove first via the "+ New cove" affordance,
// then navigate into it and create a wave there.
//
// Prereq: `make dev` must be serving the docker stack at
// http://localhost:4040 with the default seed. We use unique titles per
// run (`E2E … <timestamp>`) so re-runs don't collide with leftovers.

import { test, expect } from '@playwright/test';

test('creates a new wave from a fresh cove and navigates to it', async ({ page }) => {
  await page.goto('/calm/');

  // Step 1 — sidebar → mint a new user cove (issue #175: the system
  // cove that hosts the default Today terminal is hidden from the
  // sidebar; we always start by creating our own cove for the test).
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
