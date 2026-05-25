// E2E: NewTaskForm "Create new cove" miss-branch (#250 PR 3).
//
// Scenario covered: the user opens a fresh cove page, clicks "+ New
// wave", and types a cwd that no cove claims yet. The resolve misses,
// the radio picker appears, the user flips from the default "Existing
// cove" to "Create new cove", fills a fresh name (the deterministic
// palette color is auto-seeded — no color picker yet), and submits.
// The two-step path inside NewTaskForm runs:
//   1. POST /api/coves         → mints the new cove
//   2. POST /api/waves         → creates the wave under the new cove
//                                 with `attach_folder: true`, so the
//                                 cwd lands as a cove_folders row in
//                                 the same tx.
// After the round-trip we land on `/calm/wave/<id>`, the sidebar shows
// the just-minted cove, and the new cove's folders list contains the
// cwd we typed.
//
// Prereq: `make dev` serving http://localhost:4041 with the default
// seed. Each run uses unique titles + a unique cwd namespace so
// concurrent / repeated runs don't collide on
// cove_folders.UNIQUE(path).

import { test, expect } from '@playwright/test';

// Coves seeded (REST-direct or form-indirect) get tracked here so the
// afterEach hook can `DELETE /api/coves/<id>` them. Without cleanup,
// leftover coves accumulate and break specs that assume a zero-cove
// baseline (notably golden-path.spec.ts; #250 PR5 triage).
// `DELETE /api/coves/:id` cascades through waves → cards → terminals
// (see `delete_cove` in crates/calm-server/src/routes/coves.rs).
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

test('NewTaskForm "Create new cove" branch mints cove + claims cwd', async ({ page }) => {
  const ts = Date.now();
  const starterCoveName = `E2E newcove starter ${ts}`;

  // Step 1 — mint a "starter" cove via REST and navigate directly to
  // its page. The sidebar has no `overflow: auto`
  // (body { overflow: hidden }), so once enough coves accumulate from
  // prior runs the "+ New cove" row gets pushed outside the document
  // and Playwright cannot scroll to it. This spec doesn't exercise the
  // sidebar-create flow (`wave-create.spec.ts` owns that contract); it
  // only needs a user cove to land on so `defaultCoveId` is set. The
  // starter cove is *not* where the wave should land — the form's
  // "Create new cove" branch will mint a different one.
  const starterRes = await page.request.post('/api/coves', {
    data: { name: starterCoveName, color: '#4a8' },
    headers: { 'content-type': 'application/json' },
  });
  expect(starterRes.ok()).toBeTruthy();
  const starterCove = (await starterRes.json()) as { id: string };
  createdCoveIds.push(starterCove.id);

  await page.goto(`/calm/cove/${starterCove.id}`);
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+$/);
  // Sidebar landmark — referenced again below to assert the newly
  // minted cove shows up after the form's create-cove mutation.
  const sidebarCoves = page.getByRole('navigation', { name: 'Coves' });

  // Step 2 — expand the "+ New wave" CTA into NewTaskForm.
  await page.getByRole('button', { name: /new wave/i }).click();
  const form = page.getByRole('form', { name: /new task/i });
  await expect(form).toBeVisible();

  const title = `E2E new-cove wave ${ts}`;
  await form.getByLabel(/task description/i).fill(title);

  // Per-spec cwd namespace — `/tmp/playwright-newcove-<ts>` belongs
  // to this run alone. No prior cove claims this prefix, so resolve
  // misses and the radio picker appears.
  const cwd = `/tmp/playwright-newcove-${ts}`;
  await form.getByLabel(/working directory/i).fill(cwd);

  // The form defaults the cove choice to "Existing cove" because a
  // defaultCoveId is in play (the starter cove). We need to flip to
  // "Create new cove". Resolve debounce is 300ms; the radio shows up
  // as soon as resolveState transitions out of `resolving`. Use the
  // visible radio label as the locator — works regardless of the
  // generated useId() hash.
  const newCoveRadio = form.getByRole('radio', { name: /create new cove/i });
  await expect(newCoveRadio).toBeVisible({ timeout: 5_000 });
  await newCoveRadio.check();

  // Cove name input appears with an explicit aria-label.
  const newCoveName = `E2E newcove ${ts}`;
  const newCoveNameInput = form.getByLabel(/new cove name/i);
  await expect(newCoveNameInput).toBeVisible();
  await newCoveNameInput.fill(newCoveName);

  // Submit. The two-step (create cove → create wave) is opaque from
  // the user's POV — we just expect the URL push when both succeed.
  await form.getByRole('button', { name: /create task/i }).click();

  // Step 3 — landed on the wave detail page. ~10s for the two-step
  // round-trip + WS fanout + router push.
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+$/, { timeout: 10_000 });
  const waveId = new URL(page.url()).pathname.split('/').pop()!;

  // Title we typed renders somewhere on the wave page (cheapest "yes
  // it really mounted" check, mirrors wave-create.spec.ts). The wave
  // header today shows only the title (not the cwd); the cwd contract
  // gets pinned by the REST assertion below instead of a DOM-text
  // check that would silently lock in chrome we haven't shipped.

  // Step 4 — the new cove name is in the sidebar (the create-cove
  // mutation's onSuccess invalidate + the wave-create cache poke both
  // refresh the coves list).
  // `exact: true` excludes the per-row "Delete cove \"<name>\"" button
  // whose accessible name also contains newCoveName — without exact
  // match the locator hits both and trips Playwright's strict mode.
  await expect(
    sidebarCoves.getByRole('button', { name: newCoveName, exact: true }),
  ).toBeVisible({ timeout: 5_000 });

  // Step 5 — REST assertion: the wave actually belongs to the new
  // cove, and the new cove has the cwd attached as a folder. This is
  // the "did the kernel state match what the UI implied?" check.
  //
  // `GET /api/waves/:id` returns a `WaveDetail` envelope
  // `{ wave: {...}, cards, overlays }` — destructure the inner wave.
  const waveRes = await page.request.get(`/api/waves/${waveId}`);
  expect(waveRes.ok()).toBeTruthy();
  const { wave } = (await waveRes.json()) as {
    wave: { cove_id: string; cwd: string };
  };
  // Track the form-minted cove (distinct from the starter cove pushed
  // above) for afterEach cleanup.
  createdCoveIds.push(wave.cove_id);
  expect(wave.cwd).toBe(cwd);

  const foldersRes = await page.request.get(
    `/api/coves/${wave.cove_id}/folders`,
  );
  expect(foldersRes.ok()).toBeTruthy();
  const folders = (await foldersRes.json()) as { path: string }[];
  expect(folders.map((f) => f.path)).toContain(cwd);

  // And the cove on the wave is *not* the starter cove — confirms
  // the create-new-cove branch actually minted a fresh cove. There's
  // no GET /api/coves/:id route (only list/patch/delete), so we look
  // up the cove on the wave through the list endpoint.
  const covesRes = await page.request.get('/api/coves');
  expect(covesRes.ok()).toBeTruthy();
  const allCoves = (await covesRes.json()) as { id: string; name: string }[];
  const waveCove = allCoves.find((c) => c.id === wave.cove_id);
  expect(waveCove).toBeTruthy();
  expect(waveCove!.name).toBe(newCoveName);
});
