// E2E: NewTaskForm cwd → cove auto-match overrides defaultCoveId
// (#250 PR 3).
//
// Scenario covered: cove A owns `/srv/proj/foo`. The user opens cove
// B's page (so defaultCoveId points at B) and types a cwd under cove
// A's claim. NewTaskForm's debounced resolve lands a *hit* on A, the
// auto-match banner appears naming A, and submitting POSTs the wave
// against A — *not* B. This is the load-bearing "longest-prefix
// resolve wins over surrounding page context" contract; without it
// the user would silently land waves in the wrong cove every time
// they typed a path that another cove already owns.
//
// Implementation notes:
//   * Cove A + its folder are seeded via REST so the spec doesn't
//     depend on the sidebar's NewCove flow having a color picker.
//   * Cove B is minted via the sidebar (the standard happy-path) so
//     the page-navigation half of the scenario uses the real router
//     flow.
//   * Each run namespaces its cwd under `/srv/proj/auto-<ts>` to
//     avoid colliding with concurrent / repeated runs on
//     cove_folders.UNIQUE(path).
//
// Prereq: `make dev` serving http://localhost:4041 with the default
// seed.

import { test, expect } from '@playwright/test';

// Coves seeded via REST get tracked here so the afterEach hook can
// `DELETE /api/coves/<id>` them. Without cleanup, leftover coves
// accumulate and break specs that assume a zero-cove baseline (notably
// golden-path.spec.ts; #250 PR5 triage). `DELETE /api/coves/:id`
// cascades through waves → cards → terminals (see `delete_cove` in
// crates/calm-server/src/routes/coves.rs).
const createdCoveIds: string[] = [];

test.beforeEach(() => {
  createdCoveIds.length = 0;
});

test.afterEach(async ({ request }) => {
  for (const id of createdCoveIds) {
    const res = await request.delete(`/api/coves/${id}`);
    if (!res.ok() && res.status() !== 404) {
      throw new Error(
        `cleanup: DELETE /api/coves/${id} → ${res.status()} ${res.statusText()}`,
      );
    }
  }
  createdCoveIds.length = 0;
});

test('NewTaskForm auto-matches cwd to claiming cove (not surrounding cove)', async ({
  page,
}) => {
  const ts = Date.now();
  const coveAName = `E2E auto cove-A ${ts}`;
  const coveBName = `E2E auto cove-B ${ts}`;
  const folderPath = `/srv/proj/auto-${ts}`;
  const cwd = `${folderPath}/sub`;

  // Step 1 — seed cove A + its folder claim via REST. Cove A is the
  // *correct* destination; the test's whole point is that the form
  // notices this and routes the wave to A even though the user is
  // looking at B's page.
  const coveARes = await page.request.post('/api/coves', {
    data: { name: coveAName, color: '#5a9' },
    headers: { 'content-type': 'application/json' },
  });
  expect(coveARes.ok()).toBeTruthy();
  const coveA = (await coveARes.json()) as { id: string };
  createdCoveIds.push(coveA.id);

  const folderRes = await page.request.post(
    `/api/coves/${coveA.id}/folders`,
    {
      data: { path: folderPath },
      headers: { 'content-type': 'application/json' },
    },
  );
  expect(folderRes.ok()).toBeTruthy();

  // Step 2 — mint cove B via REST (not via the sidebar). The sidebar
  // has no `overflow: auto` (body { overflow: hidden }), so once enough
  // coves accumulate from prior runs the "+ New cove" row gets pushed
  // outside the document and Playwright cannot scroll to it. This spec
  // doesn't exercise the sidebar-create flow (`wave-create.spec.ts`
  // owns that contract); it only needs cove B to exist so we can land
  // on its page with `defaultCoveId === B.id`. REST + direct goto
  // gives identical post-conditions without depending on viewport
  // height.
  const coveBRes = await page.request.post('/api/coves', {
    data: { name: coveBName, color: '#a75' },
    headers: { 'content-type': 'application/json' },
  });
  expect(coveBRes.ok()).toBeTruthy();
  const coveB = (await coveBRes.json()) as { id: string };
  const coveBId = coveB.id;
  createdCoveIds.push(coveBId);

  await page.goto(`/calm/cove/${coveBId}`);
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

  // Step 3 — expand "+ New wave" into the form.
  await page.getByRole('button', { name: /new wave/i }).click();
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  const title = `E2E auto-match wave ${ts}`;
  await form.getByLabel(/task description/i).fill(title);
  await form.getByLabel(/working directory/i).fill(cwd);

  // Step 4 — wait for the auto-match banner. The form uses a
  // `data-testid="cove-auto-match"` testid + the visible text starts
  // with "Auto-matched to cove". Anchor on the testid so the locator
  // is robust if the surrounding copy is ever tweaked. ~5s covers the
  // 300ms debounce + resolve round-trip.
  const banner = form.getByTestId('cove-auto-match');
  await expect(banner).toBeVisible({ timeout: 5_000 });
  await expect(banner).toContainText(coveAName);
  await expect(banner).toContainText(folderPath);

  // Step 5 — submit. The form should route through `coveChoice.mode
  // === 'auto'`, posting the wave with cove_id = A (attach_folder
  // intentionally false because the cwd is already covered).
  await form.getByRole('button', { name: 'Create task', exact: true }).click();

  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/, { timeout: 10_000 });
  const waveId = new URL(page.url()).pathname.split('/').pop()!;

  // Step 6 — REST assert: wave.cove_id MUST be A (the auto-matched
  // cove), not B (the surrounding cove page). This is the contract
  // the test exists to pin.
  //
  // `GET /api/waves/:id` returns a `WaveDetail` envelope
  // `{ wave: {...}, cards, overlays }` — destructure the inner wave.
  const waveRes = await page.request.get(`/api/waves/${waveId}`);
  expect(waveRes.ok()).toBeTruthy();
  const { wave } = (await waveRes.json()) as {
    wave: { cove_id: string; cwd: string };
  };
  expect(wave.cwd).toBe(cwd);
  expect(wave.cove_id).toBe(coveA.id);
  expect(wave.cove_id).not.toBe(coveBId);

  // Step 7 — cove A's folders list still has the seeded claim and
  // nothing new (attach_folder was false → the wave-create tx did
  // *not* insert a redundant cove_folders row for `cwd`).
  const foldersRes = await page.request.get(`/api/coves/${coveA.id}/folders`);
  expect(foldersRes.ok()).toBeTruthy();
  const folders = (await foldersRes.json()) as { path: string }[];
  const paths = folders.map((f) => f.path);
  expect(paths).toContain(folderPath);
  // No new claim was minted for the descendant cwd.
  expect(paths).not.toContain(cwd);
});
