// E2E: NewTaskForm cwd → area auto-match overrides defaultAreaId
// (#250 PR 3).
//
// Scenario covered: area A owns `/srv/proj/foo`. The user opens area
// B's page (so defaultAreaId points at B) and types a cwd under area
// A's claim. NewTaskForm's debounced resolve lands a *hit* on A, the
// auto-match banner appears naming A, and submitting POSTs the track
// against A — *not* B. This is the load-bearing "cwd resolve wins over
// surrounding page context" contract; without it
// the user would silently land tracks in the wrong area every time
// they typed a path that another area already owns.
//
// Implementation notes:
//   * Area A + its folder are seeded via REST so the planner doesn't
//     depend on the sidebar's NewArea flow having a color picker.
//   * Area B is minted via the sidebar (the standard happy-path) so
//     the page-navigation half of the scenario uses the real router
//     flow.
//   * Each run namespaces its cwd under a per-run `$HOME` work tree
//     (`neige-e2e-auto-match-<ts>`) to avoid colliding with concurrent
//     / repeated runs on area_folders.UNIQUE(path).
//
// Prereq: `make dev` serving http://localhost:4041 with the default
// seed.

import { test, expect } from '@playwright/test';
import {
  attachedWorkspacePath,
  cleanupAttachedWorkspaces,
  createGitWorkTree,
  createWorkTreeSubdir,
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

test('NewTaskForm auto-matches cwd to claiming area (not surrounding area)', async ({
  page,
}) => {
  const ts = Date.now();
  const areaAName = `E2E auto area-A ${ts}`;
  const areaBName = `E2E auto area-B ${ts}`;
  // #1147 S3 — `cwd` is submitted as an *attached* workspace, so it has
  // to exist and be inside a Git work tree the kernel can see.
  // `folderPath` is the work-tree root and area A's claim; `cwd` is a
  // real directory beneath it, which is precisely the descendant shape
  // this planner is about (the claim covers the cwd, so the track-create tx
  // must NOT mint a second claim). See `helpers/attached-workspace.ts`
  // for why these live under `$HOME`.
  const folderPath = createGitWorkTree(
    attachedWorkspacePath(`neige-e2e-auto-match-${ts}`),
  );
  const cwd = createWorkTreeSubdir(folderPath, 'sub');

  // Step 1 — seed area A + its folder claim via REST. Area A is the
  // *correct* destination; the test's whole point is that the form
  // notices this and routes the track to A even though the user is
  // looking at B's page.
  const areaARes = await page.request.post('/api/areas', {
    data: { name: areaAName, color: '#5a9' },
    headers: { 'content-type': 'application/json' },
  });
  expect(areaARes.ok()).toBeTruthy();
  const areaA = (await areaARes.json()) as { id: string };
  createdAreaIds.push(areaA.id);

  const folderRes = await page.request.post(
    `/api/areas/${areaA.id}/folders`,
    {
      data: { path: folderPath },
      headers: { 'content-type': 'application/json' },
    },
  );
  expect(folderRes.ok()).toBeTruthy();

  // Step 2 — mint area B via REST (not via the sidebar). The sidebar
  // has no `overflow: auto` (body { overflow: hidden }), so once enough
  // areas accumulate from prior runs the "+ New area" row gets pushed
  // outside the document and Playwright cannot scroll to it. This planner
  // doesn't exercise the sidebar-create flow (`track-create.spec.ts`
  // owns that contract); it only needs area B to exist so we can land
  // on its page with `defaultAreaId === B.id`. REST + direct goto
  // gives identical post-conditions without depending on viewport
  // height.
  const areaBRes = await page.request.post('/api/areas', {
    data: { name: areaBName, color: '#a75' },
    headers: { 'content-type': 'application/json' },
  });
  expect(areaBRes.ok()).toBeTruthy();
  const areaB = (await areaBRes.json()) as { id: string };
  const areaBId = areaB.id;
  createdAreaIds.push(areaBId);

  await page.goto(`/calm/area/${areaBId}`);
  await expect(page).toHaveURL(/\/calm\/area\/[^/]+$/);

  // Step 3 — expand "+ New track" into the form.
  await page.getByRole('button', { name: /new track/i }).click();
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  const title = `E2E auto-match track ${ts}`;
  await form.getByLabel(/task description/i).fill(title);
  await form.getByLabel(/working directory/i).fill(cwd);

  // Step 4 — wait for the auto-match banner. The form uses a
  // `data-testid="area-auto-match"` testid + the visible text starts
  // with "Auto-matched to area". Anchor on the testid so the locator
  // is robust if the surrounding copy is ever tweaked. ~5s covers the
  // 300ms debounce + resolve round-trip.
  const banner = form.getByTestId('area-auto-match');
  await expect(banner).toBeVisible({ timeout: 5_000 });
  await expect(banner).toContainText(areaAName);
  await expect(banner).toContainText(folderPath);

  // Step 5 — submit. The form should route through `areaChoice.mode
  // === 'auto'`, posting the track with area_id = A (attach_folder
  // intentionally false because the cwd is already covered).
  await form.getByRole('button', { name: 'Create task', exact: true }).click();

  await expect(page).toHaveURL(/\/calm\/track\/[^/]+$/, { timeout: 10_000 });
  const trackId = new URL(page.url()).pathname.split('/').pop()!;

  // Step 6 — REST assert: track.area_id MUST be A (the auto-matched
  // area), not B (the surrounding area page). This is the contract
  // the test exists to pin.
  //
  // `GET /api/tracks/:id` returns a `TrackDetail` envelope
  // `{ track: {...}, cards, overlays }` — destructure the inner track.
  const trackRes = await page.request.get(`/api/tracks/${trackId}`);
  expect(trackRes.ok()).toBeTruthy();
  const { track } = (await trackRes.json()) as {
    track: { area_id: string; cwd: string };
  };
  expect(track.cwd).toBe(cwd);
  expect(track.area_id).toBe(areaA.id);
  expect(track.area_id).not.toBe(areaBId);

  // Step 7 — area A's folders list still has the seeded claim and
  // nothing new (attach_folder was false → the track-create tx did
  // *not* insert a redundant area_folders row for `cwd`).
  const foldersRes = await page.request.get(`/api/areas/${areaA.id}/folders`);
  expect(foldersRes.ok()).toBeTruthy();
  const folders = (await foldersRes.json()) as { path: string }[];
  const paths = folders.map((f) => f.path);
  expect(paths).toContain(folderPath);
  // No new claim was minted for the descendant cwd.
  expect(paths).not.toContain(cwd);
});
