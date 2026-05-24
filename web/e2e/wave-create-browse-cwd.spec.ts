// E2E: "+ New wave" → NewTaskForm → Browse… → DirectoryBrowser →
// pick a directory → cwd input reflects the picked path.
//
// Scenario covered: with the form now hosted inside a Dialog (per the
// move from inline-expansion to modal), clicking "Browse…" next to the
// cwd input pushes the DirectoryBrowser view into the dialog body via
// `useModalView()`. The user navigates the host filesystem via the
// `GET /api/fs/listdir` walker, clicks "Select this directory", and
// the picked path is written back into the cwd input. We don't test
// the full cwd → cove resolve flow here (`wave-create.spec.ts` and
// `wave-create-auto-match.spec.ts` own that); the goal is just the
// picker → input wiring.
//
// Prereq: `make dev` serving http://localhost:4040 with the default
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
import { mkdir, rm } from 'node:fs/promises';
import { homedir } from 'node:os';
import path from 'node:path';

const createdCoveIds: string[] = [];
const createdDirs: string[] = [];

test.beforeEach(() => {
  createdCoveIds.length = 0;
  createdDirs.length = 0;
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
  const coveName = `E2E browse cove ${ts}`;
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

  // Step 1 — seed a cove via REST (no sidebar dependency; this spec
  // doesn't exercise the sidebar create flow).
  const coveRes = await page.request.post('/api/coves', {
    data: { name: coveName, color: '#5a9' },
    headers: { 'content-type': 'application/json' },
  });
  expect(coveRes.ok()).toBeTruthy();
  const cove = (await coveRes.json()) as { id: string };
  createdCoveIds.push(cove.id);

  await page.goto(`/calm/cove/${cove.id}`);
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

  // Step 2 — open the New wave dialog. The CTA is the "+ New wave"
  // button on the cove page; clicking it now opens a Dialog (not an
  // inline-expanded form). The form heading "New task" still labels
  // the form region inside the dialog.
  await page.getByRole('button', { name: /new wave/i }).click();
  const dialog = page.getByRole('dialog', { name: 'New wave' });
  await expect(dialog).toBeVisible();
  const form = dialog.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  // Step 3 — click Browse… next to the cwd input. The button's
  // accessible name is its visible text ("Browse…"); scoping to the
  // dialog also keeps us clear of any "browse" matches elsewhere on
  // the page (none today, but the locator stays robust under future
  // additions).
  const browseBtn = dialog.getByRole('button', { name: /browse/i });
  await expect(browseBtn).toBeVisible();
  await browseBtn.click();

  // The dialog body has been taken over by the DirectoryBrowser. The
  // outer dialog's accessible name swaps to the pushed view's title
  // ("Choose a directory") — this is the `useModalView()` contract.
  const browserDialog = page.getByRole('dialog', { name: /choose a directory/i });
  await expect(browserDialog).toBeVisible();

  // Step 4 — the listdir endpoint defaults to $HOME (server-side), so
  // the browser opens on $HOME with our pre-created directory listed.
  // We just click into the unique entry and confirm.
  const cwdLabel = browserDialog.locator('.dirpicker-cwd');
  await expect(cwdLabel).toHaveText(home, { timeout: 5_000 });

  // Click into our pre-created directory. The listbox option's
  // accessible name is its `<button>` text; we use exact match so we
  // don't pick a sibling whose name happens to share a prefix.
  await browserDialog.getByRole('option', { name: dirName, exact: true }).click();
  await expect(cwdLabel).toHaveText(dirPath, { timeout: 5_000 });

  // Step 5 — confirm with "Select this directory". The browser view
  // pops; the dialog title swaps back to "New wave" and the cwd input
  // now carries the picked path.
  await browserDialog
    .getByRole('button', { name: /select this directory/i })
    .click();

  // Back on the normal form view — the dialog title flips back.
  await expect(page.getByRole('dialog', { name: 'New wave' })).toBeVisible();
  await expect(form.getByLabel(/working directory/i)).toHaveValue(dirPath);

  // Step 6 — finish the create flow to prove the picked path goes the
  // distance. Resolve will miss (no cove claims `/tmp/...`), and the
  // form defaults the cove choice to "Existing cove" (the surrounding
  // cove). Submit → land on the wave detail page.
  const title = `E2E browse wave ${ts}`;
  await form.getByLabel(/task description/i).fill(title);
  // The resolve debounce + cove section flicker; wait for the radio
  // group to settle into miss-mode (the picker), then the form is
  // ready to submit.
  await expect(form.getByRole('radiogroup', { name: /cove selection/i }))
    .toBeVisible({ timeout: 5_000 });

  await form.getByRole('button', { name: /create task/i }).click();
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/, { timeout: 10_000 });

  // REST assertion: the wave's cwd is the picked path. This closes the
  // loop end-to-end (Browse picked path → cwd input → POST /api/waves
  // body → wave row in the kernel).
  const waveId = new URL(page.url()).pathname.split('/').pop()!;
  const waveRes = await page.request.get(`/api/waves/${waveId}`);
  expect(waveRes.ok()).toBeTruthy();
  const { wave } = (await waveRes.json()) as {
    wave: { cove_id: string; cwd: string };
  };
  expect(wave.cwd).toBe(dirPath);
});
