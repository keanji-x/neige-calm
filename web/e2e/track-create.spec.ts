// E2E: create a track end-to-end via the NewTaskForm and land on its
// detail page.
//
// After issue #175 there is no seeded `Scratch` area in the sidebar.
// We mint our own user area first via the "+ New area" affordance,
// then navigate into it and create a track by expanding the area-page
// "+ New track" button into NewTaskForm (#250 PR 3). The form does
// the cwd → area resolve dance, but since this fresh area has no
// folder claims yet the resolve misses and we take the "existing
// area + attach_folder=true" branch (the area is preselected by the
// surrounding AreaPage).
//
// Prereq: `make dev` must be serving the docker stack at
// http://localhost:4040 with the default seed. We use unique titles
// per run (`E2E … <timestamp>`) so re-runs don't collide with
// leftovers — and a unique per-run cwd under `$HOME` (see
// `helpers/attached-workspace.ts`) so concurrent runs don't trip the
// area_folders UNIQUE(path).

import { test, expect } from '@playwright/test';
import {
  attachedWorkspacePath,
  cleanupAttachedWorkspaces,
  createGitWorkTree,
} from './helpers/attached-workspace';

// Areas seeded (directly via REST or indirectly via the sidebar UI)
// get tracked here so the afterEach hook can `DELETE /api/areas/<id>`
// them. Without cleanup, leftover areas accumulate and break specs that
// assume a zero-area baseline (notably golden-path.spec.ts; #250 PR5
// triage). `DELETE /api/areas/:id` cascades through tracks → cards →
// terminals (see `delete_area` in crates/calm-server/src/routes/areas.rs).
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

test('creates a new track from a fresh area via NewTaskForm and navigates to it', async ({ page }) => {
  await page.goto('/calm/');

  // Step 1 — sidebar → mint a new user area (issue #175).
  const sidebarAreas = page.getByRole('navigation', { name: 'Areas' });
  const areaName = `E2E area ${Date.now()}`;
  await sidebarAreas.getByRole('button', { name: /new area/i }).click();
  const nameInput = sidebarAreas.getByPlaceholder(/name/i);
  await expect(nameInput).toBeVisible();
  await nameInput.fill(areaName);
  await nameInput.press('Enter');

  // `exact: true` excludes the per-row "Delete area \"<name>\"" button
  // whose accessible name also contains areaName — without exact match
  // the locator hits both and trips Playwright's strict mode.
  const areaBtn = sidebarAreas.getByRole('button', { name: areaName, exact: true });
  await expect(areaBtn).toBeVisible();
  await areaBtn.click();
  await expect(page).toHaveURL(/\/calm\/area\/[^/]+$/);

  // Step 2 — click the "+ New track" CTA. It opens a modal Dialog
  // hosting the shared NewTaskForm (per #250 PR 3 the area page no
  // longer renders a one-line title input; all creation goes through
  // the configuration card, now wrapped in a Dialog so the cwd
  // Browse… picker can take over the modal body).
  const newTrackBtn = page.getByRole('button', { name: /new track/i });
  await expect(newTrackBtn).toBeVisible();
  await newTrackBtn.click();

  // Step 3 — the form expanded. Locate it via its accessible name
  // ("New task" — the form heading) so we don't collide with other
  // textareas/inputs on the page (none exist here today, but the
  // landmark makes the locator robust).
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  const title = `E2E track ${Date.now()}`;
  await form.getByLabel(/task description/i).fill(title);

  // Unique absolute cwd so concurrent test runs don't race on
  // area_folders.UNIQUE(path). The form will resolve this and miss
  // (no folder claim yet); the area dropdown defaults to "existing"
  // with the current area preselected (AreaPage passes
  // `defaultAreaId={area.id}`), so submit goes through with
  // `attach_folder: true`.
  //
  // #1147 S3 — the legacy `web/` NewTaskForm always puts this input's
  // value on the wire, so this test cannot take the omit-cwd managed
  // branch: the path has to be a real Git work tree the kernel can see.
  // See `helpers/attached-workspace.ts` for why it lives under `$HOME`.
  const cwd = createGitWorkTree(attachedWorkspacePath(`neige-e2e-track-create-${Date.now()}`));
  await form.getByLabel(/working directory/i).fill(cwd);

  // Submit via the Create task button. (Pressing Enter on the cwd
  // input would also submit — the keyboard variant lives in the a11y
  // test.)
  await form.getByRole('button', { name: 'Create task', exact: true }).click();

  // Step 4 — URL transitions to /calm/track/<id> and the track page
  // mounts. We allow up to ~10s for the round-trip (kernel insert +
  // folder attach + WS fanout + router push).
  await expect(page).toHaveURL(/\/calm\/track\/[^/]+$/, { timeout: 10_000 });

  // The track title we just submitted should appear on the page; this
  // is the cheapest "the track really rendered" assertion.
  await expect(page.getByText(title, { exact: false }).first()).toBeVisible();

  // Resolve the sidebar-minted area id via the track-detail endpoint and
  // hand it off to the afterEach cleanup hook. We don't get it from
  // sidebar markup because the sidebar `<button>` carries only the area
  // name, not its id; the track we just created links back to its area
  // through `track.area_id`, so we cash that out into the cleanup list.
  const trackId = new URL(page.url()).pathname.split('/').pop()!;
  const trackRes = await page.request.get(`/api/tracks/${trackId}`);
  expect(trackRes.ok()).toBeTruthy();
  const { track } = (await trackRes.json()) as { track: { area_id: string } };
  createdAreaIds.push(track.area_id);
});
