// E2E: NewTaskForm "Create new area" miss-branch (#250 PR 3).
//
// Scenario covered: the user opens a fresh area page, clicks "+ New
// track", and types a cwd that no area claims yet. The resolve misses,
// the radio picker appears, the user flips from the default "Existing
// area" to "Create new area", fills a fresh name (the deterministic
// palette color is auto-seeded — no color picker yet), and submits.
// The two-step path inside NewTaskForm runs:
//   1. POST /api/areas         → mints the new area
//   2. POST /api/tracks         → creates the track under the new area
//                                 with `attach_folder: true`, so the
//                                 cwd lands as a area_folders row in
//                                 the same tx.
// After the round-trip we land on `/calm/track/<id>`, the sidebar shows
// the just-minted area, and the new area's folders list contains the
// cwd we typed.
//
// Prereq: `make dev` serving http://localhost:4041 with the default
// seed. Each run uses unique titles + a unique cwd namespace so
// concurrent / repeated runs don't collide on
// area_folders.UNIQUE(path).

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

test('NewTaskForm "Create new area" branch mints area + claims cwd', async ({ page }) => {
  const ts = Date.now();
  const starterAreaName = `E2E newcove starter ${ts}`;

  // Step 1 — mint a "starter" area via REST and navigate directly to
  // its page. The sidebar has no `overflow: auto`
  // (body { overflow: hidden }), so once enough areas accumulate from
  // prior runs the "+ New area" row gets pushed outside the document
  // and Playwright cannot scroll to it. This spec doesn't exercise the
  // sidebar-create flow (`track-create.spec.ts` owns that contract); it
  // only needs a user area to land on so `defaultAreaId` is set. The
  // starter area is *not* where the track should land — the form's
  // "Create new area" branch will mint a different one.
  const starterRes = await page.request.post('/api/areas', {
    data: { name: starterAreaName, color: '#4a8' },
    headers: { 'content-type': 'application/json' },
  });
  expect(starterRes.ok()).toBeTruthy();
  const starterArea = (await starterRes.json()) as { id: string };
  createdAreaIds.push(starterArea.id);

  await page.goto(`/calm/area/${starterArea.id}`);
  await expect(page).toHaveURL(/\/calm\/area\/[^/]+$/);
  // Sidebar landmark — referenced again below to assert the newly
  // minted area shows up after the form's create-area mutation.
  const sidebarAreas = page.getByRole('navigation', { name: 'Areas' });

  // Step 2 — expand the "+ New track" CTA into NewTaskForm.
  await page.getByRole('button', { name: /new track/i }).click();
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  const title = `E2E new-area track ${ts}`;
  await form.getByLabel(/task description/i).fill(title);

  // Per-spec cwd namespace — `$HOME/neige-e2e-new-area-<ts>` belongs
  // to this run alone. No prior area claims this prefix, so resolve
  // misses and the radio picker appears.
  //
  // #1147 S3 — this path is submitted as an *attached* workspace (and
  // then becomes the new area's folder claim), so it has to exist and
  // be a Git work tree the kernel can see. See
  // `helpers/attached-workspace.ts` for why it lives under `$HOME`.
  const cwd = createGitWorkTree(attachedWorkspacePath(`neige-e2e-new-area-${ts}`));
  await form.getByLabel(/working directory/i).fill(cwd);

  // The form defaults the area choice to "Existing area" because a
  // defaultAreaId is in play (the starter area). We need to flip to
  // "Create new area". Resolve debounce is 300ms; the radio shows up
  // as soon as resolveState transitions out of `resolving`. Use the
  // visible radio label as the locator — works regardless of the
  // generated useId() hash.
  const newAreaRadio = form.getByRole('radio', { name: /create new area/i });
  await expect(newAreaRadio).toBeVisible({ timeout: 5_000 });
  await newAreaRadio.check();

  // Area name input appears with an explicit aria-label.
  const newAreaName = `E2E newcove ${ts}`;
  const newAreaNameInput = form.getByLabel(/new area name/i);
  await expect(newAreaNameInput).toBeVisible();
  await newAreaNameInput.fill(newAreaName);

  // Submit. The two-step (create area → create track) is opaque from
  // the user's POV — we just expect the URL push when both succeed.
  await form.getByRole('button', { name: /create task/i }).click();

  // Step 3 — landed on the track detail page. ~10s for the two-step
  // round-trip + WS fanout + router push.
  await expect(page).toHaveURL(/\/calm\/track\/[^/]+$/, { timeout: 10_000 });
  const trackId = new URL(page.url()).pathname.split('/').pop()!;

  // Title we typed renders somewhere on the track page (cheapest "yes
  // it really mounted" check, mirrors track-create.spec.ts). The track
  // header today shows only the title (not the cwd); the cwd contract
  // gets pinned by the REST assertion below instead of a DOM-text
  // check that would silently lock in chrome we haven't shipped.

  // Step 4 — the new area name is in the sidebar (the create-area
  // mutation's onSuccess invalidate + the track-create cache poke both
  // refresh the areas list).
  // `exact: true` excludes the per-row "Delete area \"<name>\"" button
  // whose accessible name also contains newAreaName — without exact
  // match the locator hits both and trips Playwright's strict mode.
  await expect(
    sidebarAreas.getByRole('button', { name: newAreaName, exact: true }),
  ).toBeVisible({ timeout: 5_000 });

  // Step 5 — REST assertion: the track actually belongs to the new
  // area, and the new area has the cwd attached as a folder. This is
  // the "did the kernel state match what the UI implied?" check.
  //
  // `GET /api/tracks/:id` returns a `TrackDetail` envelope
  // `{ track: {...}, cards, overlays }` — destructure the inner track.
  const trackRes = await page.request.get(`/api/tracks/${trackId}`);
  expect(trackRes.ok()).toBeTruthy();
  const { track } = (await trackRes.json()) as {
    track: { area_id: string; cwd: string };
  };
  // Track the form-minted area (distinct from the starter area pushed
  // above) for afterEach cleanup.
  createdAreaIds.push(track.area_id);
  expect(track.cwd).toBe(cwd);

  const foldersRes = await page.request.get(
    `/api/areas/${track.area_id}/folders`,
  );
  expect(foldersRes.ok()).toBeTruthy();
  const folders = (await foldersRes.json()) as { path: string }[];
  expect(folders.map((f) => f.path)).toContain(cwd);

  // And the area on the track is *not* the starter area — confirms
  // the create-new-area branch actually minted a fresh area. There's
  // no GET /api/areas/:id route (only list/patch/delete), so we look
  // up the area on the track through the list endpoint.
  const areasRes = await page.request.get('/api/areas');
  expect(areasRes.ok()).toBeTruthy();
  const allAreas = (await areasRes.json()) as { id: string; name: string }[];
  const trackArea = allAreas.find((c) => c.id === track.area_id);
  expect(trackArea).toBeTruthy();
  expect(trackArea!.name).toBe(newAreaName);
});
