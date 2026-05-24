// E2E: NewTaskForm "Use a different cove" override on top of an
// auto-matched cwd (#250 PR 3 fix-loop "B2" — the user can reject the
// auto-match and re-pick).
//
// Scenario covered: cove A owns `/srv/proj/foo-<ts>`. The user opens
// cove A's *own* page and types a cwd under that folder. The form
// auto-matches to A and locks the cove choice. The user clicks "Use
// a different cove", the radio picker reappears, they flip to "Create
// new cove", swap the cwd to a *non-overlapping* path so no 409 fires,
// fill the new cove name, and submit. The wave should land in the
// brand-new cove C (NOT A), with the new path attached.
//
// Without B2 the auto-match locks A in permanently and the user has
// no escape hatch when they want to mint a fresh cove for a path
// that happens to fall under an existing claim.
//
// Prereq: `make dev` serving http://localhost:4040 with the default
// seed.

import { test, expect } from '@playwright/test';

test('NewTaskForm "Use a different cove" lets user override auto-match → new cove', async ({
  page,
}) => {
  const ts = Date.now();
  const coveAName = `E2E override cove-A ${ts}`;
  const newCoveName = `E2E override cove-C ${ts}`;
  const coveAFolder = `/srv/proj/foo-override-${ts}`;
  const initialCwd = `${coveAFolder}/sub`;
  // Non-overlapping cwd for the actual submit — independent
  // namespace so it can't collide with cove A's claim or any other
  // spec's cwds.
  const finalCwd = `/tmp/playwright-override-${ts}`;

  // Step 1 — seed cove A + its folder claim via REST. We'll navigate
  // into A so defaultCoveId === A.id, which is what triggers the
  // override fallback path inside `onOverrideAutoMatch` (it picks
  // defaultCoveId as the "existing" fallback).
  const coveARes = await page.request.post('/api/coves', {
    data: { name: coveAName, color: '#79c' },
    headers: { 'content-type': 'application/json' },
  });
  expect(coveARes.ok()).toBeTruthy();
  const coveA = (await coveARes.json()) as { id: string };

  const folderRes = await page.request.post(
    `/api/coves/${coveA.id}/folders`,
    {
      data: { path: coveAFolder },
      headers: { 'content-type': 'application/json' },
    },
  );
  expect(folderRes.ok()).toBeTruthy();

  // Step 2 — navigate to cove A's page directly (no need to recreate
  // it via the sidebar since we just minted it via REST — the sidebar
  // refreshes via the coves WS event + useCovesQuery).
  await page.goto(`/calm/cove/${coveA.id}`);
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);

  // Step 3 — expand the form, type the under-A cwd.
  await page.getByRole('button', { name: /new wave/i }).click();
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  const title = `E2E override wave ${ts}`;
  await form.getByLabel(/task description/i).fill(title);
  const cwdInput = form.getByLabel(/working directory/i);
  await cwdInput.fill(initialCwd);

  // Step 4 — auto-match banner shows naming A.
  const banner = form.getByTestId('cove-auto-match');
  await expect(banner).toBeVisible({ timeout: 5_000 });
  await expect(banner).toContainText(coveAName);

  // Step 5 — the override button exists inside the banner and is
  // clickable.
  const overrideBtn = form.getByRole('button', { name: /use a different cove/i });
  await expect(overrideBtn).toBeVisible();
  await expect(overrideBtn).toBeEnabled();
  await overrideBtn.click();

  // Step 6 — the radio picker reappears. Flip to "Create new cove".
  const newCoveRadio = form.getByRole('radio', { name: /create new cove/i });
  await expect(newCoveRadio).toBeVisible();
  await newCoveRadio.check();

  // Fill the new cove name (palette color is seeded automatically).
  const newCoveNameInput = form.getByLabel(/new cove name/i);
  await expect(newCoveNameInput).toBeVisible();
  await newCoveNameInput.fill(newCoveName);

  // Step 7 — swap the cwd to a path nobody owns. After the override
  // flag is latched, the resolveState transitions (idle/miss/hit) no
  // longer rewrite coveChoice — so even though this new path will
  // resolve to a miss, the "new cove" pick stays.
  await cwdInput.fill(finalCwd);
  // Give the debounce window a beat to settle (300ms) so the resolve
  // re-fires against the new cwd and we know the override latch held.
  // Wait for the resolving spinner to clear into the miss-branch
  // picker, which means the new cove radio still shows checked.
  await expect(newCoveRadio).toBeChecked();

  // Step 8 — submit. Two-step (POST cove → POST wave with
  // attach_folder=true) should both succeed.
  await form.getByRole('button', { name: /create task/i }).click();

  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/, { timeout: 10_000 });
  const waveId = new URL(page.url()).pathname.split('/').pop()!;

  // Step 9 — REST assert: wave belongs to the brand-new cove C, not A.
  //
  // `GET /api/waves/:id` returns a `WaveDetail` envelope
  // `{ wave: {...}, cards, overlays }` — destructure the inner wave.
  const waveRes = await page.request.get(`/api/waves/${waveId}`);
  expect(waveRes.ok()).toBeTruthy();
  const { wave } = (await waveRes.json()) as {
    wave: { cove_id: string; cwd: string };
  };
  expect(wave.cwd).toBe(finalCwd);
  expect(wave.cove_id).not.toBe(coveA.id);

  // Look up the cove name through GET /api/coves (no GET-by-id route).
  const covesRes = await page.request.get('/api/coves');
  expect(covesRes.ok()).toBeTruthy();
  const allCoves = (await covesRes.json()) as { id: string; name: string }[];
  const waveCove = allCoves.find((c) => c.id === wave.cove_id);
  expect(waveCove).toBeTruthy();
  expect(waveCove!.name).toBe(newCoveName);

  // The new cove's folders list contains the final cwd (attach_folder
  // landed it inside the wave-create tx).
  const foldersRes = await page.request.get(
    `/api/coves/${wave.cove_id}/folders`,
  );
  expect(foldersRes.ok()).toBeTruthy();
  const folders = (await foldersRes.json()) as { path: string }[];
  expect(folders.map((f) => f.path)).toContain(finalCwd);
});
