// E2E: "+ New track" → NewTaskForm → Browse… → DirectoryBrowser →
// pick a directory → cwd input reflects the picked path.
//
// Scenario covered: with the form now hosted inside a Dialog (per the
// move from inline-expansion to modal), clicking "Browse…" next to the
// cwd input pushes the DirectoryBrowser view into the dialog body via
// `useModalView()`. The user navigates the host filesystem via the
// `GET /api/fs/listdir` walker, clicks "Select this directory", and
// the picked path is written back into the cwd input. We don't test
// the full cwd → area resolve flow here (`track-create.spec.ts` and
// `track-create-auto-match.spec.ts` own that); the goal is just the
// picker → input wiring.
//
// Prereq: `make dev` serving http://localhost:4041 with the default
// seed. The server's `listdir` endpoint walks the kernel container's
// filesystem; the docker-compose mounts `$HOME` at the same path
// inside the container (see docker-compose.yml ~L133), so a directory
// we mkdir under `$HOME` on the host is visible to the kernel. /tmp
// is NOT shared (the container has its own ephemeral /tmp), which
// would cause the picker test to time out hunting for an entry that
// doesn't exist from the kernel's POV.
//
// Each run uses a unique on-disk directory (`$HOME/playwright-browse-<ts>`)
// so concurrent / repeated runs don't collide. The directory is
// cleaned up in afterEach.

import { test, expect } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { mkdir, rm } from 'node:fs/promises';
import { homedir } from 'node:os';
import path from 'node:path';

const createdAreaIds: string[] = [];
const createdDirs: string[] = [];

test.beforeEach(() => {
  createdAreaIds.length = 0;
  createdDirs.length = 0;
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
  // Tear down the temp dirs we minted — leave the host tidy.
  for (const dir of createdDirs) {
    await rm(dir, { recursive: true, force: true });
  }
  createdDirs.length = 0;
});

test('Browse… picks a directory from disk and writes it into the cwd input', async ({
  page,
}) => {
  const ts = Date.now();
  const areaName = `E2E browse area ${ts}`;
  // Pre-create a real on-disk directory under $HOME so the listdir
  // walker (running inside the kernel container) can find it via the
  // docker-compose $HOME bind-mount. The directory name is unique
  // per run so the assertion that we click on *this* entry is robust
  // against any siblings that happen to exist under $HOME.
  const home = homedir();
  const dirName = `playwright-browse-${ts}`;
  const dirPath = path.join(home, dirName);
  await mkdir(dirPath, { recursive: true });
  createdDirs.push(dirPath);
  // #1147 S3 — Step 6 below submits this path as the track's `cwd`, i.e.
  // the *attached* workspace branch, which the kernel now requires to
  // be inside a Git work tree (`validate_attached_workspace`). A bare
  // `mkdir` is enough for the Browse… listing but not for the submit,
  // so make the directory a repository root. No commit is needed: the
  // kernel's check is `git rev-parse --show-toplevel`, which succeeds
  // on a fresh empty repository.
  execFileSync('git', ['init', '--quiet', dirPath], { stdio: 'ignore' });

  // Step 1 — seed an area via REST (no sidebar dependency; this planner
  // doesn't exercise the sidebar create flow).
  const areaRes = await page.request.post('/api/areas', {
    data: { name: areaName, color: '#5a9' },
    headers: { 'content-type': 'application/json' },
  });
  expect(areaRes.ok()).toBeTruthy();
  const area = (await areaRes.json()) as { id: string };
  createdAreaIds.push(area.id);

  await page.goto(`/calm/area/${area.id}`);
  await expect(page).toHaveURL(/\/calm\/area\/[^/]+$/);

  // Step 2 — open the New track dialog. The CTA is the "+ New track"
  // button on the area page; clicking it now opens a Dialog (not an
  // inline-expanded form). The form heading "New task" still labels
  // the form region inside the dialog.
  await page.getByRole('button', { name: /new track/i }).click();
  const dialog = page.getByRole('dialog', { name: 'New track' });
  await expect(dialog).toBeVisible();
  const form = dialog.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  // Step 3 — click Browse… next to the cwd input. The button's
  // accessible name is its visible text "Browse…" (U+2026), matched
  // with `exact: true`. NOT a /browse/i regex: the area select's
  // `.calm-select-trigger` button (#891) surfaces in Chromium's AX
  // tree named by its cloned selected-option content — here the area
  // "E2E browse area ..." — which strict-mode-collides a substring
  // match. `exact: true` is the regression sentinel: it also excludes
  // any future button whose name merely CONTAINS "Browse…". Same rule
  // for every in-dialog button locator — calm-select triggers are
  // buttons named by the selected option, so match names exactly.
  const browseBtn = dialog.getByRole('button', { name: 'Browse…', exact: true });
  await expect(browseBtn).toBeVisible();
  await browseBtn.click();

  // The dialog body has been taken over by the DirectoryBrowser. The
  // outer dialog's accessible name swaps to the pushed view's title
  // ("Choose a directory") — this is the `useModalView()` contract.
  const browserDialog = page.getByRole('dialog', { name: /choose a directory/i });
  await expect(browserDialog).toBeVisible();

  // Step 4 — the listdir endpoint defaults to $HOME (server-side), so
  // the browser opens on $HOME with our pre-created directory listed
  // and reflected in the editable path input.
  const pathInput = browserDialog.getByLabel('Directory path');
  await expect(pathInput).toHaveValue(`${home}/`, { timeout: 5_000 });

  // Click into our pre-created directory. The listbox option's
  // accessible name is its `<button>` text; we use exact match so we
  // don't pick a sibling whose name happens to share a prefix.
  await browserDialog.getByRole('option', { name: dirName, exact: true }).click();
  await expect(pathInput).toHaveValue(`${dirPath}/`, { timeout: 5_000 });

  // Step 5 — confirm with "Select this directory". The browser view
  // pops; the dialog title swaps back to "New track" and the cwd input
  // now carries the picked path.
  await browserDialog
    .getByRole('button', { name: /select this directory/i })
    .click();

  // Back on the normal form view — the dialog title flips back.
  await expect(page.getByRole('dialog', { name: 'New track' })).toBeVisible();
  await expect(form.getByLabel(/working directory/i)).toHaveValue(dirPath);

  // Step 6 — finish the create flow to prove the picked path goes the
  // distance. Resolve will miss (no area claims `/tmp/...`), and the
  // form defaults the area choice to "Existing area" (the surrounding
  // area). Submit → land on the track detail page.
  const title = `E2E browse track ${ts}`;
  await form.getByLabel(/task description/i).fill(title);
  // The resolve debounce + area section flicker; wait for the radio
  // group to settle into miss-mode (the picker), then the form is
  // ready to submit.
  await expect(form.getByRole('radiogroup', { name: /area selection/i }))
    .toBeVisible({ timeout: 5_000 });

  await form.getByRole('button', { name: /create task/i }).click();
  await expect(page).toHaveURL(/\/calm\/track\/[^/]+$/, { timeout: 10_000 });

  // REST assertion: the track's cwd is the picked path. This closes the
  // loop end-to-end (Browse picked path → cwd input → POST /api/tracks
  // body → track row in the kernel).
  const trackId = new URL(page.url()).pathname.split('/').pop()!;
  const trackRes = await page.request.get(`/api/tracks/${trackId}`);
  expect(trackRes.ok()).toBeTruthy();
  const { track } = (await trackRes.json()) as {
    track: { area_id: string; cwd: string };
  };
  expect(track.cwd).toBe(dirPath);
});
