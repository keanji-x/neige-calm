// E2E: NewTaskForm FolderConflict 409 → friendly error naming the
// owning area (#250 PR 3 fix-loop "B1" — `formatSubmitError` resolves
// `conflict.area_id` → area name via the local `useAreasQuery` cache).
//
// Scenario covered: area A owns a per-run path under `$HOME`. The user
// opens area B's page, types that exact path as cwd, overrides the
// auto-match banner via "Use a different area", picks area B in the
// radio + dropdown (so `attach_folder: true` will fire against B),
// and submits. The server 409s with a `{area_id: A, conflict_path,
// conflict_kind: "equal"}` body; the form must render an error that
// names area A (not a raw UUID), keeps the form mounted on B's page,
// and leaves the inputs editable so the user can pivot.
//
// Without the B1 fix, the error would read "claimed by another area"
// regardless of cache state — this test specifically locks in the
// area-name lookup path.
//
// Prereq: `make dev` serving http://localhost:4041 with the default
// seed.

import { test, expect } from '@playwright/test';
import {
  attachedWorkspacePath,
  cleanupAttachedWorkspaces,
  createGitWorkTree,
} from './helpers/attached-workspace';

// Areas seeded via REST get tracked here so the afterEach hook can
// `DELETE /api/areas/<id>` them. Without cleanup, leftover areas
// accumulate and break specs that assume a zero-area baseline (notably
// golden-path.spec.ts; #250 PR5 triage). `DELETE /api/areas/:id`
// cascades through tracks → cards → terminals (see `delete_area` in
// crates/calm-server/src/routes/areas.rs).
const createdAreaIds: string[] = [];

test.beforeEach(() => {
  createdAreaIds.length = 0;
});

test.afterEach(async ({ request }) => {
  for (const id of createdAreaIds) {
    const res = await request.delete(`/api/areas/${id}`);
    if (!res.ok() && res.status() !== 404) {
      throw new Error(
        `cleanup: DELETE /api/areas/${id} → ${res.status()} ${res.statusText()}`,
      );
    }
  }
  createdAreaIds.length = 0;
  cleanupAttachedWorkspaces();
});

test('NewTaskForm surfaces conflicting area name in 409 error', async ({ page }) => {
  const ts = Date.now();
  const areaAName = `E2E conflict area-A ${ts}`;
  const areaBName = `E2E conflict area-B ${ts}`;
  // #1147 S3 — this path is submitted, so it has to clear
  // `validate_attached_workspace` (exists + inside a Git work tree)
  // BEFORE the request can reach the folder-claim scan that produces
  // the 409 this test is about. A non-existent path would now 400 with
  // "attached workspace: … does not exist" — a different failure that
  // does not name area A, so the test would fail rather than silently
  // pass on the wrong error. See `helpers/attached-workspace.ts`.
  const blockedPath = createGitWorkTree(
    attachedWorkspacePath(`neige-e2e-conflict-${ts}`),
  );

  // Step 1 — seed area A + the conflicting folder claim via REST.
  // Use a distinctive name so the assertion can grep for it without
  // false positives.
  const areaARes = await page.request.post('/api/areas', {
    data: { name: areaAName, color: '#c97' },
    headers: { 'content-type': 'application/json' },
  });
  expect(areaARes.ok()).toBeTruthy();
  const areaA = (await areaARes.json()) as { id: string };
  createdAreaIds.push(areaA.id);

  const folderRes = await page.request.post(
    `/api/areas/${areaA.id}/folders`,
    {
      data: { path: blockedPath },
      headers: { 'content-type': 'application/json' },
    },
  );
  expect(folderRes.ok()).toBeTruthy();

  // Step 2 — mint area B via REST and navigate directly to its page.
  // The sidebar has no `overflow: auto` (body { overflow: hidden }),
  // so once enough areas accumulate from prior runs the "+ New area"
  // row gets pushed outside the document and Playwright cannot scroll
  // to it. This test doesn't exercise the sidebar-create flow
  // (`track-create.spec.ts` owns that contract); it only needs area B
  // to exist as the surrounding page for the form. REST + direct goto
  // gives identical post-conditions without depending on viewport
  // height.
  const areaBRes = await page.request.post('/api/areas', {
    data: { name: areaBName, color: '#b86' },
    headers: { 'content-type': 'application/json' },
  });
  expect(areaBRes.ok()).toBeTruthy();
  const areaB = (await areaBRes.json()) as { id: string };
  createdAreaIds.push(areaB.id);

  await page.goto(`/calm/area/${areaB.id}`);
  await expect(page).toHaveURL(/\/calm\/area\/[^/]+$/);
  const areaUrlBeforeSubmit = page.url();

  // Step 3 — expand the form and fill it in.
  await page.getByRole('button', { name: /new track/i }).click();
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  const title = `E2E conflict track ${ts}`;
  await form.getByLabel(/task description/i).fill(title);
  await form.getByLabel(/working directory/i).fill(blockedPath);

  // Step 4 — resolve hits → auto-match banner appears naming area A.
  // We need to override it so we can force the 409 by posting with
  // attach_folder=true against area B.
  const banner = form.getByTestId('area-auto-match');
  await expect(banner).toBeVisible({ timeout: 5_000 });
  await expect(banner).toContainText(areaAName);

  // Click "Use a different area" (PR 3 fix-loop B2 surface).
  await form.getByRole('button', { name: 'Use a different area', exact: true }).click();

  // The radio picker reappears. Make sure "Existing area" is picked
  // (it's the default after override when defaultAreaId is set).
  // Then ensure the dropdown is set to area B explicitly.
  const existingRadio = form.getByRole('radio', { name: /existing area/i });
  await expect(existingRadio).toBeVisible();
  await existingRadio.check();
  // The combobox lists every user area; pick B by label.
  await form.getByRole('combobox').selectOption({ label: areaBName });

  // Step 5 — submit. The track-create POST against B with
  // attach_folder=true should server-side fail with a 409 because A
  // already owns the same path.
  await form.getByRole('button', { name: 'Create task', exact: true }).click();

  // Step 6 — error appears inline via role="alert". The text must
  // contain area A's *name* (B1 fix), the conflicting path, and must
  // *not* contain area A's UUID (proves the name lookup hit, not the
  // fallback).
  const errorAlert = form.getByRole('alert');
  await expect(errorAlert).toBeVisible({ timeout: 10_000 });
  await expect(errorAlert).toContainText(areaAName);
  await expect(errorAlert).toContainText(blockedPath);
  await expect(errorAlert).not.toContainText(areaA.id);

  // Step 7 — URL hasn't pushed; we're still on area B's page with the
  // form mounted. Inputs are still editable (no submit-locked state
  // left behind) — the user can pivot to a different path.
  expect(page.url()).toBe(areaUrlBeforeSubmit);
  await expect(form).toBeVisible();
  const cwdInput = form.getByLabel(/working directory/i);
  await expect(cwdInput).toBeEnabled();
  // Inline edit smoke: typing extends the value, confirms not locked.
  await cwdInput.fill(`${blockedPath}-pivot`);
  await expect(cwdInput).toHaveValue(`${blockedPath}-pivot`);
});
