// E2E: NewTaskForm "Use a different area" override on top of an
// auto-matched cwd (#250 PR 3 fix-loop "B2" — the user can reject the
// auto-match and re-pick).
//
// Scenario covered: area A owns `/srv/proj/foo-<ts>`. The user opens
// area A's *own* page and types a cwd under that folder. The form
// auto-matches to A and locks the area choice. The user clicks "Use
// a different area", the radio picker reappears, they flip to "Create
// new area", swap the cwd to a *non-overlapping* path so no 409 fires,
// fill the new area name, and submit. The track should land in the
// brand-new area C (NOT A), with the new path attached.
//
// Without B2 the auto-match locks A in permanently and the user has
// no escape hatch when they want to mint a fresh area for a path
// that happens to fall under an existing claim.
//
// Prereq: `make dev` serving http://localhost:4041 with the default
// seed.

import { test, expect } from '@playwright/test';
import {
  attachedWorkspacePath,
  cleanupAttachedWorkspaces,
  createGitWorkTree,
} from './helpers/attached-workspace';

// Areas seeded (REST-direct or form-indirect) get tracked here so the
// afterEach hook can `DELETE /api/areas/<id>` them. Without cleanup,
// leftover areas accumulate and break specs that assume a zero-area
// baseline (notably golden-path.spec.ts; #250 PR5 triage).
// `DELETE /api/areas/:id` cascades through tracks → cards → terminals
// (see `delete_area` in crates/calm-server/src/routes/areas.rs).
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

test('NewTaskForm "Use a different area" lets user override auto-match → new area', async ({
  page,
}) => {
  const ts = Date.now();
  const areaAName = `E2E override area-A ${ts}`;
  const newAreaName = `E2E override area-C ${ts}`;
  // `areaAFolder` / `initialCwd` stay pure path strings on purpose:
  // `initialCwd` is only ever *typed* (to make the auto-match banner
  // fire) and is swapped out before submit, and a `area_folders` claim
  // carries no filesystem contract. Only the path that reaches
  // `POST /api/tracks` has to exist on disk.
  const areaAFolder = `/srv/proj/foo-override-${ts}`;
  const initialCwd = `${areaAFolder}/sub`;
  // Non-overlapping cwd for the actual submit — independent
  // namespace so it can't collide with area A's claim or any other
  // test's cwds.
  //
  // #1147 S3 — this one IS submitted as an *attached* workspace, so it
  // must exist and be a Git work tree the kernel can see. See
  // `helpers/attached-workspace.ts` for why it lives under `$HOME`.
  const finalCwd = createGitWorkTree(
    attachedWorkspacePath(`neige-e2e-override-${ts}`),
  );

  // Step 1 — seed area A + its folder claim via REST. We'll navigate
  // into A so defaultAreaId === A.id, which is what triggers the
  // override fallback path inside `onOverrideAutoMatch` (it picks
  // defaultAreaId as the "existing" fallback).
  const areaARes = await page.request.post('/api/areas', {
    data: { name: areaAName, color: '#79c' },
    headers: { 'content-type': 'application/json' },
  });
  expect(areaARes.ok()).toBeTruthy();
  const areaA = (await areaARes.json()) as { id: string };
  createdAreaIds.push(areaA.id);

  const folderRes = await page.request.post(
    `/api/areas/${areaA.id}/folders`,
    {
      data: { path: areaAFolder },
      headers: { 'content-type': 'application/json' },
    },
  );
  expect(folderRes.ok()).toBeTruthy();

  // Step 2 — navigate to area A's page directly (no need to recreate
  // it via the sidebar since we just minted it via REST — the sidebar
  // refreshes via the areas WS event + useAreasQuery).
  await page.goto(`/calm/area/${areaA.id}`);
  await expect(page).toHaveURL(/\/calm\/area\/[^/]+$/);

  // Step 3 — expand the form, type the under-A cwd.
  await page.getByRole('button', { name: /new track/i }).click();
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  const title = `E2E override track ${ts}`;
  await form.getByLabel(/task description/i).fill(title);
  const cwdInput = form.getByLabel(/working directory/i);
  await cwdInput.fill(initialCwd);

  // Step 4 — auto-match banner shows naming A.
  const banner = form.getByTestId('area-auto-match');
  await expect(banner).toBeVisible({ timeout: 5_000 });
  await expect(banner).toContainText(areaAName);

  // Step 5 — the override button exists inside the banner and is
  // clickable.
  const overrideBtn = form.getByRole('button', { name: 'Use a different area', exact: true });
  await expect(overrideBtn).toBeVisible();
  await expect(overrideBtn).toBeEnabled();
  await overrideBtn.click();

  // Step 6 — the radio picker reappears. Flip to "Create new area".
  const newAreaRadio = form.getByRole('radio', { name: /create new area/i });
  await expect(newAreaRadio).toBeVisible();
  await newAreaRadio.check();

  // Fill the new area name (palette color is seeded automatically).
  const newAreaNameInput = form.getByLabel(/new area name/i);
  await expect(newAreaNameInput).toBeVisible();
  await newAreaNameInput.fill(newAreaName);

  // Step 7 — swap the cwd to a path nobody owns. After the override
  // flag is latched, the resolveState transitions (idle/miss/hit) no
  // longer rewrite areaChoice — so even though this new path will
  // resolve to a miss, the "new area" pick stays.
  await cwdInput.fill(finalCwd);
  // Give the debounce window a beat to settle (300ms) so the resolve
  // re-fires against the new cwd and we know the override latch held.
  // Wait for the resolving spinner to clear into the miss-branch
  // picker, which means the new area radio still shows checked.
  await expect(newAreaRadio).toBeChecked();

  // Step 8 — submit. Two-step (POST area → POST track with
  // attach_folder=true) should both succeed.
  await form.getByRole('button', { name: 'Create task', exact: true }).click();

  await expect(page).toHaveURL(/\/calm\/track\/[^/]+$/, { timeout: 10_000 });
  const trackId = new URL(page.url()).pathname.split('/').pop()!;

  // Step 9 — REST assert: track belongs to the brand-new area C, not A.
  //
  // `GET /api/tracks/:id` returns a `TrackDetail` envelope
  // `{ track: {...}, cards, overlays }` — destructure the inner track.
  const trackRes = await page.request.get(`/api/tracks/${trackId}`);
  expect(trackRes.ok()).toBeTruthy();
  const { track } = (await trackRes.json()) as {
    track: { area_id: string; cwd: string };
  };
  // Track the form-minted area C (distinct from REST-seeded area A
  // already pushed above) for afterEach cleanup.
  createdAreaIds.push(track.area_id);
  expect(track.cwd).toBe(finalCwd);
  expect(track.area_id).not.toBe(areaA.id);

  // Look up the area name through GET /api/areas (no GET-by-id route).
  const areasRes = await page.request.get('/api/areas');
  expect(areasRes.ok()).toBeTruthy();
  const allAreas = (await areasRes.json()) as { id: string; name: string }[];
  const trackArea = allAreas.find((c) => c.id === track.area_id);
  expect(trackArea).toBeTruthy();
  expect(trackArea!.name).toBe(newAreaName);

  // The new area's folders list contains the final cwd (attach_folder
  // landed it inside the track-create tx).
  const foldersRes = await page.request.get(
    `/api/areas/${track.area_id}/folders`,
  );
  expect(foldersRes.ok()).toBeTruthy();
  const folders = (await foldersRes.json()) as { path: string }[];
  expect(folders.map((f) => f.path)).toContain(finalCwd);
});
