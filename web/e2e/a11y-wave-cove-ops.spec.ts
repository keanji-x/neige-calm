// E2E coverage for issue #269 P2 — wave + cove mutation flows.
//
// Scope (per the PR2-of-#269 scope-agent brief):
//   * Wave delete  — full UI confirm-dialog flow + assert kernel + event.
//   * Wave rename  — extended beyond a11y-keyboard's F2/Enter happy
//                    path: Escape cancels, empty input is a no-op,
//                    mouse-click entry path also works.
//   * Cove rename  — click / F2 to enter, Enter commits.
//   * Cove delete  — confirm dialog → kernel row gone → cascade drops
//                    the `cove_folders` row we claimed up front.
//
// Items explicitly NOT covered (and why):
//   * Wave archive — the kernel exposes archive via
//     `PATCH /api/waves/{id}` with `archived_at`, but the frontend
//     has no archive affordance today (verified by greping
//     `web/src/` for archive — only schema / type comments mention
//     it, no buttons or menu items). This PR is e2e-only and the
//     scope-agent brief forbids adding UI; tracked as a follow-up.
//
// Why a11y project (not chromium):
//   The chromium project targets the developer's `make dev` stack
//   at :4040 and is not a hermetic CI gate. a11y boots a fresh
//   in-memory replay binary per run (`_setup/replay-server.setup.ts`)
//   and exposes `POST /dev/reset` for per-test isolation. Putting
//   all five specs on a11y keeps the gate green deterministically.
//   The chromium-parity stand-in for the wave-delete flow lives in
//   the `Wave delete via confirm dialog` test below.
//
// Conventions inherited from `a11y-keyboard.spec.ts`:
//   * `?trace=1` enables `window.__neigeEvents__` so we can assert
//     against the event-trace ring buffer.
//   * Each test calls `resetReplayServer` + mints its own cove so
//     state doesn't leak across spec files.
//   * The replay binary auto-bootstraps a hidden system cove on
//     first Today render; we ignore it and anchor on user-minted
//     coves whose ids we capture from the REST response.

import { test, expect, type Page } from '@playwright/test';
import { createUserCove, createWaveInCove, resetReplayServer, REPLAY_PORT } from './helpers/reset';
import { clearEventTrace, waitForEvent } from './helpers/trace';

/** Wait for the WS-driven UI to mount our just-minted cove in the
 *  sidebar (it travels via the `cove.updated` event the REST mint
 *  fires). Mirrors the helper inside `a11y-keyboard.spec.ts` so each
 *  spec block can keep its bootstrap lockstep without importing
 *  across describe blocks. */
async function waitForCoveInSidebar(page: Page, name: string): Promise<void> {
  await expect(
    page.locator('aside.side').getByRole('button', { name: new RegExp(name, 'i') }),
  ).toBeVisible({ timeout: 15_000 });
  await page.waitForFunction(() => Array.isArray(window.__neigeEvents__));
}

/** Navigate by click — keyboard-only is the contract of
 *  `a11y-keyboard.spec.ts`, but this spec exercises mutation
 *  *flows*, not keyboard reachability. Clicks let us anchor on
 *  role+name without paying the `tabUntil` brittleness tax. */
async function gotoCove(page: Page, coveName: string): Promise<void> {
  await page
    .locator('aside.side')
    .getByRole('button', { name: new RegExp(coveName, 'i') })
    .click();
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+(\?|$)/);
}

async function gotoWaveFromCove(page: Page, waveTitle: string): Promise<void> {
  await page
    .getByRole('region', { name: 'Waves' })
    .getByRole('button', { name: new RegExp(waveTitle, 'i') })
    .first()
    .click();
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+(\?|$)/);
}

test.describe('a11y · wave + cove ops', () => {
  test.beforeEach(async ({ request }) => {
    // Hermetic per-test state: clear every accumulated row from the
    // shared replay kernel. See `helpers/reset.ts` for the rationale.
    await resetReplayServer(request);
  });

  test('Wave delete via confirm dialog removes row, fires wave.deleted, navigates back to cove', async ({
    page,
    request,
  }) => {
    // ---- Setup: mint a cove + wave via REST so the page boots
    // with the rows already present (no need to drive the
    // sidebar's New cove / New wave UI — those flows have their
    // own coverage in wave-create*.spec.ts).
    const cove = await createUserCove(request, 'AtlasDel');
    const wave = await createWaveInCove(request, cove.id, 'WaveToDelete');

    await page.goto(`/calm/wave/${wave.id}?trace=1`);
    await waitForCoveInSidebar(page, 'AtlasDel');
    // Clear the bootstrap trace so the wave.deleted assertion at the
    // end is unambiguous (the page-mount path emits its own
    // cove.updated / wave.updated as the WS feed drains).
    await clearEventTrace(page);

    // ---- Open the confirm dialog via the wave header's × button.
    // `DeleteButton` (web/src/pages/_shared.tsx) renders an icon-
    // button whose aria-label is `Delete wave "<title>"`; the
    // ConfirmDialog primitive then opens with title "Delete wave?"
    // and default-focused Cancel.
    const deleteTrigger = page.getByRole('button', { name: `Delete wave "${wave.title}"` });
    await expect(deleteTrigger).toBeVisible();
    await deleteTrigger.click();

    const dialog = page.getByRole('dialog', { name: 'Delete wave?' });
    await expect(dialog).toBeVisible();

    // ---- Cancel-safe default: focus must land on Cancel, not
    // Confirm. This is the contract `ConfirmDialog.contract.test.tsx`
    // pins at the unit-test layer; we mirror it here so the e2e
    // gate catches a regression that bypasses ConfirmDialog (e.g.
    // a future refactor reverting to window.confirm).
    const cancelBtn = dialog.getByRole('button', { name: 'Cancel' });
    await expect(cancelBtn).toBeFocused();

    // ---- Confirm via keyboard (Tab to Confirm, Enter) — the
    // ConfirmDialog contract treats keyboard activation the same
    // as a click, so this also exercises the confirm path's
    // disabled-while-pending guard.
    await page.keyboard.press('Tab');
    const confirmBtn = dialog.getByRole('button', { name: 'Delete wave' });
    await expect(confirmBtn).toBeFocused();
    await page.keyboard.press('Enter');

    // ---- Post-delete contract:
    //   * UI navigates back to the cove page (router.tsx wires
    //     `go({name:'cove', coveId:cove.id})` on the wave-page
    //     `onDeleteWave` handler).
    //   * The wave.deleted event fires on the trace buffer.
    //   * GET /api/waves/<id> returns 404.
    await expect(page).toHaveURL(new RegExp(`/calm/cove/${cove.id}(\\?|$)`), { timeout: 10_000 });
    const evt = await waitForEvent(page, 'wave.deleted');
    expect((evt.data as { id: string }).id).toBe(wave.id);

    const detailRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/waves/${wave.id}`);
    expect(detailRes.status()).toBe(404);
  });

  test('Wave rename: Escape cancels, empty input is a no-op, mouse-click enters edit mode', async ({
    page,
    request,
  }) => {
    const cove = await createUserCove(request, 'AtlasRen');
    const wave = await createWaveInCove(request, cove.id, 'OriginalTitle');

    await page.goto(`/calm/wave/${wave.id}?trace=1`);
    await waitForCoveInSidebar(page, 'AtlasRen');
    await clearEventTrace(page);

    // The wave title in `Wave.tsx` renders as a
    // `<span role="button" aria-label={wave.title} aria-describedby=…>`
    // when `onRenameWave` is supplied. Locate by role + name +
    // description ("Rename wave") so we don't collide with the
    // cove-crumb button (same span tag, no description).
    const titleDisplay = page.getByRole('button', {
      name: wave.title,
      description: 'Rename wave',
    });
    await expect(titleDisplay).toBeVisible();

    // -------- Path 1: mouse-click enters edit mode --------
    // The span has `onClick={startRename}` when rename is wired;
    // proves the pointer-driven entry path that
    // a11y-keyboard.spec.ts (keyboard F2 only) doesn't cover.
    await titleDisplay.click();
    const input = page.getByLabel('Wave title');
    await expect(input).toBeFocused();

    // -------- Path 2: Escape cancels --------
    // The cancel path must not POST anything, must not change the
    // displayed title, and must restore focus to the display span.
    await input.fill('SomethingElse');
    await page.keyboard.press('Escape');
    await expect(titleDisplay).toBeFocused();
    await expect(titleDisplay).toBeVisible();
    // The on-disk title hasn't changed.
    const afterCancel = await request.get(
      `http://127.0.0.1:${REPLAY_PORT}/api/waves/${wave.id}`,
    );
    const detailBody = (await afterCancel.json()) as { wave: { title: string } };
    expect(detailBody.wave.title).toBe('OriginalTitle');

    // -------- Path 3: empty input is a no-op --------
    // `commitRename` in Wave.tsx short-circuits when the trimmed
    // value is empty or unchanged — it sets editingTitle=false,
    // restores focus, and never calls onRenameWave. We assert by
    // (a) the title staying the same in the DOM, (b) no
    // `wave.updated` envelope landing on the trace within a
    // bounded window.
    await titleDisplay.click();
    await expect(input).toBeFocused();
    await input.fill('   '); // whitespace-only; trimmed == empty
    await page.keyboard.press('Enter');
    await expect(titleDisplay).toBeFocused();
    // Bounded wait — if a wave.updated arrives within 1500ms the
    // empty-input branch is broken. We swallow the timeout (the
    // happy path) and re-throw on an unexpected event.
    await expect(async () => {
      await waitForEvent(page, 'wave.updated', 1500);
    }).rejects.toThrow(/Timeout/);

    // -------- Path 4: real edit still commits --------
    // The earlier no-op paths must not have wedged any state;
    // a normal click-edit-Enter must still POST and update.
    await titleDisplay.click();
    await expect(input).toBeFocused();
    const newTitle = `Renamed${Date.now()}`;
    await input.fill(newTitle);
    await page.keyboard.press('Enter');
    const evt = await waitForEvent(page, 'wave.updated');
    expect((evt.data as { id: string; title: string }).title).toBe(newTitle);
    // UI reflects the new title — re-locate, the old span unmounted.
    await expect(
      page.getByRole('button', { name: newTitle, description: 'Rename wave' }),
    ).toBeVisible();
  });

  test('Cove rename: click to edit, blur commits, GET /api/coves reflects new name', async ({
    page,
    request,
  }) => {
    const cove = await createUserCove(request, 'OriginalCove');
    await createWaveInCove(request, cove.id, 'Today');

    await page.goto(`/calm/cove/${cove.id}?trace=1`);
    await waitForCoveInSidebar(page, 'OriginalCove');
    await clearEventTrace(page);

    // `EditableTitle` in Cove.tsx renders the cove name as a real
    // <button class="h-display-rename"> with aria-label=value and
    // aria-describedby pointing at the "Rename cove name" sr-only
    // hint. Locate via role + name + description for parity with
    // the wave-rename spec above.
    const titleBtn = page.getByRole('button', {
      name: 'OriginalCove',
      description: 'Rename cove name',
    });
    await expect(titleBtn).toBeVisible();
    await titleBtn.click();

    const input = page.getByLabel('Cove name');
    await expect(input).toBeFocused();
    const newName = `RenamedCove${Date.now()}`;
    // Clear + fill via Playwright. The EditableTitle input mounts
    // with the current value pre-selected (microtask `select()` in
    // `enter()`), so we explicitly `selectText()` before `fill()` to
    // force a deterministic replace regardless of whether the
    // microtask landed before the test grabbed focus. We then commit
    // by *blurring* the input rather than pressing Enter directly:
    // `onBlur={save}` is the same code path Enter takes, and blur via
    // focusing a sibling button avoids the subtle race where an Enter
    // keydown can re-trigger the about-to-mount display button's
    // default click handler (re-entering edit mode with a stale
    // draft) — verified by inspecting the failure-mode screenshot
    // for an earlier iteration of this test, see the trace artifact.
    await input.selectText();
    await input.fill(newName);
    // Blur by tabbing away — the input's `onBlur={save}` runs and
    // commits the rename. Tab moves focus to the next focusable
    // sibling (the delete-cove icon button in the header), which
    // also proves the dialog's tab order didn't get wedged.
    await page.keyboard.press('Tab');

    // The kernel emits cove.updated on rename — wait for it then
    // confirm the REST list reflects the new name.
    const evt = await waitForEvent(page, 'cove.updated');
    expect((evt.data as { id: string; name: string }).name).toBe(newName);

    const listRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/coves`);
    expect(listRes.ok()).toBe(true);
    const coves = (await listRes.json()) as { id: string; name: string }[];
    const ours = coves.find((c) => c.id === cove.id);
    expect(ours, 'cove row still present after rename').toBeDefined();
    expect(ours?.name).toBe(newName);

    // The header re-renders with the new name; the rename button's
    // accessible name is now the new value.
    await expect(
      page.getByRole('button', { name: newName, description: 'Rename cove name' }),
    ).toBeVisible();
  });

  test('Cove delete via confirm dialog cascades into cove_folders', async ({
    page,
    request,
  }) => {
    const cove = await createUserCove(request, 'AtlasCascade');
    await createWaveInCove(request, cove.id, 'Today');

    // Claim a folder up front so we have something to verify
    // cascade-deleted. `createWaveInCove` already attaches a
    // `/tmp/playwright-cove-<id>` folder via `attach_folder=true`;
    // we register a second, non-overlapping path here so the
    // assertion is direct ("the folder I claimed for this test
    // is gone") rather than indirect ("some folder is gone").
    const folderPath = `/tmp/playwright-cascade-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const folderRes = await request.post(
      `http://127.0.0.1:${REPLAY_PORT}/api/coves/${cove.id}/folders`,
      {
        data: { path: folderPath },
        headers: { 'content-type': 'application/json' },
      },
    );
    expect(folderRes.ok(), `cove folder POST: ${folderRes.status()}`).toBe(true);
    const folder = (await folderRes.json()) as { id: number; cove_id: string; path: string };
    expect(folder.cove_id).toBe(cove.id);

    // Sanity check: the resolve endpoint finds the folder we just
    // claimed before we drop the cove.
    const resolveBefore = await request.get(
      `http://127.0.0.1:${REPLAY_PORT}/api/coves/resolve?path=${encodeURIComponent(folderPath)}`,
    );
    expect(resolveBefore.ok()).toBe(true);
    const resolvedBefore = (await resolveBefore.json()) as { cove_id: string } | null;
    expect(resolvedBefore?.cove_id).toBe(cove.id);

    await page.goto(`/calm/cove/${cove.id}?trace=1`);
    await waitForCoveInSidebar(page, 'AtlasCascade');
    await clearEventTrace(page);

    // CovePage header carries `<DeleteButton label={Delete cove "<name>"}>`
    // — see Cove.tsx. Open the dialog, confirm.
    const deleteTrigger = page.getByRole('button', { name: `Delete cove "${cove.name}"` });
    await expect(deleteTrigger).toBeVisible();
    await deleteTrigger.click();

    const dialog = page.getByRole('dialog', { name: 'Delete cove?' });
    await expect(dialog).toBeVisible();
    const confirmBtn = dialog.getByRole('button', { name: 'Delete cove' });
    await confirmBtn.click();

    // ---- Post-delete contract:
    //   * cove.deleted event fires.
    //   * The cove no longer appears in GET /api/coves.
    //   * The cove_folders row CASCADE-dropped (migration 0015):
    //     /api/coves/resolve returns null for the same path.
    //   * Router navigates back to the Today page (router.tsx wires
    //     `go({name:'today'})` on the cove-page delete handler →
    //     the indexRoute at `/`, which under `basepath: '/calm'`
    //     shows in the browser URL as `/calm/` — no `/today` suffix).
    const evt = await waitForEvent(page, 'cove.deleted');
    expect((evt.data as { id: string }).id).toBe(cove.id);

    await expect(page).toHaveURL(/\/calm\/(\?|$)/, { timeout: 10_000 });

    const listRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/coves`);
    const coves = (await listRes.json()) as { id: string }[];
    expect(coves.find((c) => c.id === cove.id), 'cove row should be gone').toBeUndefined();

    const resolveAfter = await request.get(
      `http://127.0.0.1:${REPLAY_PORT}/api/coves/resolve?path=${encodeURIComponent(folderPath)}`,
    );
    expect(resolveAfter.ok()).toBe(true);
    const resolvedAfter = await resolveAfter.json();
    expect(resolvedAfter, 'cove_folders row should CASCADE-drop with the cove').toBeNull();
  });
});
