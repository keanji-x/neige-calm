// E2E: NewTaskForm FolderConflict 409 → friendly error naming the
// owning cove (#250 PR 3 fix-loop "B1" — `formatSubmitError` resolves
// `conflict.cove_id` → cove name via the local `useCovesQuery` cache).
//
// Scenario covered: cove A owns `/srv/proj/blocked-<ts>`. The user
// opens cove B's page, types that exact path as cwd, overrides the
// auto-match banner via "Use a different cove", picks cove B in the
// radio + dropdown (so `attach_folder: true` will fire against B),
// and submits. The server 409s with a `{cove_id: A, conflict_path,
// conflict_kind: "equal"}` body; the form must render an error that
// names cove A (not a raw UUID), keeps the form mounted on B's page,
// and leaves the inputs editable so the user can pivot.
//
// Without the B1 fix, the error would read "claimed by another cove"
// regardless of cache state — this spec specifically locks in the
// cove-name lookup path.
//
// Prereq: `make dev` serving http://localhost:4040 with the default
// seed.

import { test, expect } from '@playwright/test';

test('NewTaskForm surfaces conflicting cove name in 409 error', async ({ page }) => {
  const ts = Date.now();
  const coveAName = `E2E conflict cove-A ${ts}`;
  const coveBName = `E2E conflict cove-B ${ts}`;
  const blockedPath = `/srv/proj/blocked-${ts}`;

  // Step 1 — seed cove A + the conflicting folder claim via REST.
  // Use a distinctive name so the assertion can grep for it without
  // false positives.
  const coveARes = await page.request.post('/api/coves', {
    data: { name: coveAName, color: '#c97' },
    headers: { 'content-type': 'application/json' },
  });
  expect(coveARes.ok()).toBeTruthy();
  const coveA = (await coveARes.json()) as { id: string };

  const folderRes = await page.request.post(
    `/api/coves/${coveA.id}/folders`,
    {
      data: { path: blockedPath },
      headers: { 'content-type': 'application/json' },
    },
  );
  expect(folderRes.ok()).toBeTruthy();

  // Step 2 — mint cove B via REST and navigate directly to its page.
  // The sidebar has no `overflow: auto` (body { overflow: hidden }),
  // so once enough coves accumulate from prior runs the "+ New cove"
  // row gets pushed outside the document and Playwright cannot scroll
  // to it. This spec doesn't exercise the sidebar-create flow
  // (`wave-create.spec.ts` owns that contract); it only needs cove B
  // to exist as the surrounding page for the form. REST + direct goto
  // gives identical post-conditions without depending on viewport
  // height.
  const coveBRes = await page.request.post('/api/coves', {
    data: { name: coveBName, color: '#b86' },
    headers: { 'content-type': 'application/json' },
  });
  expect(coveBRes.ok()).toBeTruthy();
  const coveB = (await coveBRes.json()) as { id: string };

  await page.goto(`/calm/cove/${coveB.id}`);
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);
  const coveUrlBeforeSubmit = page.url();

  // Step 3 — expand the form and fill it in.
  await page.getByRole('button', { name: /new wave/i }).click();
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  const title = `E2E conflict wave ${ts}`;
  await form.getByLabel(/task description/i).fill(title);
  await form.getByLabel(/working directory/i).fill(blockedPath);

  // Step 4 — resolve hits → auto-match banner appears naming cove A.
  // We need to override it so we can force the 409 by posting with
  // attach_folder=true against cove B.
  const banner = form.getByTestId('cove-auto-match');
  await expect(banner).toBeVisible({ timeout: 5_000 });
  await expect(banner).toContainText(coveAName);

  // Click "Use a different cove" (PR 3 fix-loop B2 surface).
  await form.getByRole('button', { name: /use a different cove/i }).click();

  // The radio picker reappears. Make sure "Existing cove" is picked
  // (it's the default after override when defaultCoveId is set).
  // Then ensure the dropdown is set to cove B explicitly.
  const existingRadio = form.getByRole('radio', { name: /existing cove/i });
  await expect(existingRadio).toBeVisible();
  await existingRadio.check();
  // The combobox lists every user cove; pick B by label.
  await form.getByRole('combobox').selectOption({ label: coveBName });

  // Step 5 — submit. The wave-create POST against B with
  // attach_folder=true should server-side fail with a 409 because A
  // already owns the same path.
  await form.getByRole('button', { name: /create task/i }).click();

  // Step 6 — error appears inline via role="alert". The text must
  // contain cove A's *name* (B1 fix), the conflicting path, and must
  // *not* contain cove A's UUID (proves the name lookup hit, not the
  // fallback).
  const errorAlert = form.getByRole('alert');
  await expect(errorAlert).toBeVisible({ timeout: 10_000 });
  await expect(errorAlert).toContainText(coveAName);
  await expect(errorAlert).toContainText(blockedPath);
  await expect(errorAlert).not.toContainText(coveA.id);

  // Step 7 — URL hasn't pushed; we're still on cove B's page with the
  // form mounted. Inputs are still editable (no submit-locked state
  // left behind) — the user can pivot to a different path.
  expect(page.url()).toBe(coveUrlBeforeSubmit);
  await expect(form).toBeVisible();
  const cwdInput = form.getByLabel(/working directory/i);
  await expect(cwdInput).toBeEnabled();
  // Inline edit smoke: typing extends the value, confirms not locked.
  await cwdInput.fill(`${blockedPath}-pivot`);
  await expect(cwdInput).toHaveValue(`${blockedPath}-pivot`);
});
