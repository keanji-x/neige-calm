// E2E: create a wave end-to-end via the NewTaskForm and land on its
// detail page.
//
// After issue #175 there is no seeded `Scratch` cove in the sidebar.
// We mint our own user cove first via the "+ New cove" affordance,
// then navigate into it and create a wave by expanding the cove-page
// "+ New wave" button into NewTaskForm (#250 PR 3). The form does
// the cwd → cove resolve dance, but since this fresh cove has no
// folder claims yet the resolve misses and we take the "existing
// cove + attach_folder=true" branch (the cove is preselected by the
// surrounding CovePage).
//
// Prereq: `make dev` must be serving the docker stack at
// http://localhost:4040 with the default seed. We use unique titles
// per run (`E2E … <timestamp>`) so re-runs don't collide with
// leftovers — and unique cwds (`/tmp/playwright-e2e-<ts>`) so
// concurrent runs don't trip the cove_folders UNIQUE(path).

import { test, expect } from '@playwright/test';

test('creates a new wave from a fresh cove via NewTaskForm and navigates to it', async ({ page }) => {
  await page.goto('/calm/');

  // Step 1 — sidebar → mint a new user cove (issue #175).
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

  // Step 2 — click the "+ New wave" CTA. It expands into NewTaskForm
  // inline (per #250 PR 3 the cove page no longer renders a one-line
  // title input; all creation goes through the configuration card).
  const newWaveBtn = page.getByRole('button', { name: /new wave/i });
  await expect(newWaveBtn).toBeVisible();
  await newWaveBtn.click();

  // Step 3 — the form expanded. Locate it via its accessible name
  // ("New task" — the form heading) so we don't collide with other
  // textareas/inputs on the page (none exist here today, but the
  // landmark makes the locator robust).
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  const title = `E2E wave ${Date.now()}`;
  await form.getByLabel(/task description/i).fill(title);

  // Unique absolute cwd so concurrent test runs don't race on
  // cove_folders.UNIQUE(path). The form will resolve this and miss
  // (no folder claim yet); the cove dropdown defaults to "existing"
  // with the current cove preselected (CovePage passes
  // `defaultCoveId={cove.id}`), so submit goes through with
  // `attach_folder: true`.
  const cwd = `/tmp/playwright-e2e-${Date.now()}`;
  await form.getByLabel(/working directory/i).fill(cwd);

  // Submit via the Create task button. (Pressing Enter on the cwd
  // input would also submit — the keyboard variant lives in the a11y
  // spec.)
  await form.getByRole('button', { name: /create task/i }).click();

  // Step 4 — URL transitions to /calm/wave/<id> and the wave page
  // mounts. We allow up to ~10s for the round-trip (kernel insert +
  // folder attach + WS fanout + router push).
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/, { timeout: 10_000 });

  // The wave title we just submitted should appear on the page; this
  // is the cheapest "the wave really rendered" assertion.
  await expect(page.getByText(title, { exact: false }).first()).toBeVisible();
});
