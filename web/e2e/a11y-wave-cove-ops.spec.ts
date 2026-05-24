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
    // microtask landed before the test grabbed focus. We commit by
    // *blurring* the input (Tab-away) rather than pressing Enter —
    // this exercises the `onBlur={save}` commit path specifically.
    // The Enter-commit path is covered by the dedicated regression
    // test below ("Cove rename: Enter then Tab — no second PATCH with
    // stale name (issue #288)"), which pins that the keyboard commit
    // doesn't leak a second PATCH via the Enter-keyup synthetic-click
    // race (issue #288, fixed in PR #292).
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

  // -----------------------------------------------------------------------
  // Rename UI-surface propagation tests.
  //
  // The block below pins that a cove / wave rename propagates to *every*
  // UI surface that names the entity — not just the kernel row + the page
  // header (which the existing tests above already cover). New surfaces:
  //
  //   * Sidebar cove entry  (the user-reported bug surface — see #288.
  //                          Passes here; see the test's own comment for
  //                          why no `test.fail()` annotation is used.)
  //   * Cove page wave list (rename on the wave detail page → the row in
  //                          the Cove page reflects the new name)
  //   * Wave breadcrumb back-link to cove (rename the cove → the wave
  //                          page's cove crumb updates)
  //   * Cove-list cache invalidation after nav-away/nav-back
  //
  // Each test resets state in the spec-level beforeEach and mints its
  // own cove + wave so the assertions don't share fate with prior tests.
  // -----------------------------------------------------------------------

  test('Cove rename: sidebar entry reflects new name', async ({ page, request }) => {
    // Issue #288 — user reported that after renaming a cove via the
    // cove-header inline rename, the sidebar entry still shows the
    // OLD name even though `cove.updated` fires and REST `GET
    // /api/coves` reflects the new name. The kernel half is covered
    // by the existing "click to edit, blur commits, GET /api/coves
    // reflects new name" test above; this test pins the UI half
    // specifically — the sidebar's cove-nav button text.
    //
    // In the hermetic a11y replay environment this assertion CURRENTLY
    // passes (the WebSocket → eventBridge → React-Query invalidation
    // path runs end-to-end on every event). The user's report came
    // against the production bundle; if a regression slips the
    // invalidation off the `cove.updated` arm — or if a future memo
    // anywhere on the `useCovesQuery` → Sidebar render path traps the
    // stale value — this test turns red.
    //
    // `test.fail()` is NOT applied because the test passes in this
    // harness as of the bug-report date (2026-05-24). Playwright
    // would flip an `expected-failure` annotation to a CI failure on
    // an unexpected pass, which would break the gate the moment the
    // hermetic env diverges from production reproduction. The issue
    // (#288) tracks the live-app reproduction separately.
    //
    // See: https://github.com/keanji-x/neige-calm/issues/288
    //
    // Multi-cove preamble: mint two siblings so the sidebar renders a
    // proper list. The user's report came against a workspace with
    // multiple coves; a single-cove sidebar is a degenerate case that
    // some failure modes (e.g. memoized-by-length list) could miss.
    await createUserCove(request, 'OtherCoveA');
    const cove = await createUserCove(request, 'SidebarStaleCove');
    await createUserCove(request, 'OtherCoveB');
    await createWaveInCove(request, cove.id, 'Today');

    // Land on Today first then navigate into the cove via the sidebar
    // — mirrors the real user flow (the reporter doesn't deep-link to
    // a cove URL, they click the sidebar entry). If the bug is
    // navigation-history sensitive (e.g. a stale memo retained across
    // the route boundary), the deep-link path would mask it.
    await page.goto('/calm/?trace=1');
    await waitForCoveInSidebar(page, 'SidebarStaleCove');
    await gotoCove(page, 'SidebarStaleCove');
    await clearEventTrace(page);

    // Drive the rename through the same UI path as the existing
    // "click → blur commits" test above — this is the surface the
    // user reported the bug against.
    const titleBtn = page.getByRole('button', {
      name: 'SidebarStaleCove',
      description: 'Rename cove name',
    });
    await titleBtn.click();

    const input = page.getByLabel('Cove name');
    await expect(input).toBeFocused();
    const newName = `SidebarFreshCove${Date.now()}`;
    await input.selectText();
    await input.fill(newName);
    // Blur commits via the same `onBlur={save}` code path as Enter; we
    // tab away to mimic the user's natural mouse-driven workflow.
    await page.keyboard.press('Tab');

    // Wait for the kernel to confirm the rename landed. If this times
    // out the bug we're pinning has shifted shape (kernel-side
    // regression) and the test will fail for the wrong reason — make
    // the failure mode loud rather than silently flaky.
    const evt = await waitForEvent(page, 'cove.updated');
    expect((evt.data as { id: string; name: string }).name).toBe(newName);

    // The actual pin, two assertions:
    //   1. The OLD name is no longer in the sidebar.
    //   2. The NEW name IS in the sidebar.
    //
    // Why both: the bug is specifically "old name persists in sidebar"
    // — checking new-name-visible alone would false-pass if the sidebar
    // rendered both rows side-by-side. Checking old-name-absent alone
    // would false-pass during the brief moment before the new row
    // renders. Both together pin the precise contract the user
    // expects.
    //
    // Bounded timeout (1.5s) so a slow refetch doesn't false-fail; the
    // cove.updated event has already landed so this is purely about
    // the React-Query → Sidebar render path. If the production-only
    // bug ever surfaces in this harness (see #288), this assertion
    // is what turns red.
    await expect(
      page.locator('aside.side').getByRole('button', { name: /SidebarStaleCove/i }),
      'old cove name must disappear from sidebar after rename',
    ).toHaveCount(0, { timeout: 1_500 });
    await expect(
      page.locator('aside.side').getByRole('button', { name: new RegExp(newName, 'i') }),
      'new cove name must appear in sidebar after rename',
    ).toBeVisible({ timeout: 1_500 });
  });

  test('Cove rename: wave page breadcrumb reflects new name', async ({ page, request }) => {
    // The wave page's header carries a "back to cove" breadcrumb
    // (`<button class="wave-cove">`) that displays the cove name. When
    // the user renames the cove from the cove page, then navigates to
    // a wave, the breadcrumb should display the new name (covered by
    // the React-Query invalidation on `cove.updated`). This test
    // exercises the linked-surface case: rename the cove from its own
    // header, navigate into a wave, assert the crumb shows the new name.
    const cove = await createUserCove(request, 'CrumbCoveOld');
    const wave = await createWaveInCove(request, cove.id, 'WorkWave');

    // Land on Today first so the WS handshake completes against a route
    // whose initial-data queries don't race the post-reset event-id
    // reset (see `replay::reset_from_fixture` — `/dev/reset` resets
    // `sqlite_sequence`, leaving any WS client with a stale cursor).
    // Deep-linking directly to `/calm/cove/<id>` after reset can land
    // before the WS resync, with the page either showing
    // "Connecting…" indefinitely or rendering with a stale React-Query
    // cove cache.
    await page.goto('/calm/?trace=1');
    await waitForCoveInSidebar(page, 'CrumbCoveOld');
    await gotoCove(page, 'CrumbCoveOld');
    await clearEventTrace(page);

    // Rename via the same flow as the existing cove-rename test.
    const titleBtn = page.getByRole('button', {
      name: 'CrumbCoveOld',
      description: 'Rename cove name',
    });
    await titleBtn.click();
    const input = page.getByLabel('Cove name');
    await expect(input).toBeFocused();
    const newName = `CrumbCoveNew${Date.now()}`;
    await input.selectText();
    await input.fill(newName);
    await page.keyboard.press('Tab');

    // Wait for the kernel confirmation before navigating — otherwise
    // the wave page would race the rename request.
    const evt = await waitForEvent(page, 'cove.updated');
    expect((evt.data as { id: string; name: string }).name).toBe(newName);

    // Navigate to the wave detail via the cove-page wave list (mirrors
    // user click flow + avoids the cold goto's lazy-WaveGrid compile
    // hit that pushes a fresh `page.goto` past Playwright's default
    // 30s timeout in this hermetic Vite-on-cargo stack).
    await gotoWaveFromCove(page, wave.title);

    // Find the breadcrumb back-link button. It has the new cove name
    // as its accessible name (text content). Scope by class to
    // disambiguate from any other "<cove name>" button on the page.
    await expect(
      page.locator('button.wave-cove', { hasText: newName }),
    ).toBeVisible({ timeout: 8_000 });
  });

  test('Cove rename: nav away + back, sidebar shows new name (no stale cache)', async ({
    page,
    request,
  }) => {
    // Cache-invalidation flavor of the surface test. Even if the
    // immediate `cove.updated` → sidebar path were broken, a navigation
    // round-trip should at minimum trigger a refetch of `['coves']`
    // when the user lands back on a route that depends on it. Today's
    // route reads the same cove query, so a Today → Cove → rename →
    // Today round trip should leave the sidebar fresh.
    //
    // This overlaps with the sidebar-rename test above but adds the
    // route-boundary dimension: a refetch-on-cove.updated regression
    // and a re-render-on-route-change regression would each show up
    // differently across these two tests.
    const cove = await createUserCove(request, 'NavBackCoveOld');
    await createWaveInCove(request, cove.id, 'Today');

    await page.goto(`/calm/cove/${cove.id}?trace=1`);
    await waitForCoveInSidebar(page, 'NavBackCoveOld');
    await clearEventTrace(page);

    const titleBtn = page.getByRole('button', {
      name: 'NavBackCoveOld',
      description: 'Rename cove name',
    });
    await titleBtn.click();
    const input = page.getByLabel('Cove name');
    await expect(input).toBeFocused();
    const newName = `NavBackCoveNew${Date.now()}`;
    await input.selectText();
    await input.fill(newName);
    await page.keyboard.press('Tab');

    const evt = await waitForEvent(page, 'cove.updated');
    expect((evt.data as { id: string; name: string }).name).toBe(newName);

    // Navigate to Today and back. The Sidebar's Today button is the
    // only Sidebar-navigation 'nav' with that name.
    await page
      .locator('aside.side')
      .getByRole('button', { name: 'Today', exact: true })
      .click();
    await expect(page).toHaveURL(/\/calm\/?(\?|$)/);

    // Return to the cove. The sidebar entry should now reflect the
    // new name — either the cove.updated path repaired itself across
    // route changes, or the route boundary forced a re-render.
    await expect(
      page.locator('aside.side').getByRole('button', { name: new RegExp(newName, 'i') }),
    ).toBeVisible({ timeout: 8_000 });
  });

  test('Wave rename: wave row in cove page reflects new title', async ({ page, request }) => {
    // The Cove page's Waves list (rendered inside <section
    // aria-label="Waves">) is a wave-rename surface that the existing
    // wave-rename test doesn't cover — it only asserts the header.
    // Rename the wave from its detail page, navigate back to the cove
    // page, and assert the row's accessible name reflects the new
    // title.
    const cove = await createUserCove(request, 'WaveRenameCove');
    const wave = await createWaveInCove(request, cove.id, 'WaveOriginalTitle');

    // Land on Today first then navigate via UI — same rationale as
    // the cove-breadcrumb test above (avoid the WS-resync race after
    // `/dev/reset` resets event-id sequence).
    await page.goto('/calm/?trace=1');
    await waitForCoveInSidebar(page, 'WaveRenameCove');
    await gotoCove(page, 'WaveRenameCove');
    await gotoWaveFromCove(page, wave.title);
    await clearEventTrace(page);

    // Drive the rename through the wave-header inline edit, mirroring
    // the existing "Wave rename" test above.
    const titleDisplay = page.getByRole('button', {
      name: wave.title,
      description: 'Rename wave',
    });
    await titleDisplay.click();
    const input = page.getByLabel('Wave title');
    await expect(input).toBeFocused();
    const newTitle = `WaveRenamed${Date.now()}`;
    await input.fill(newTitle);
    await page.keyboard.press('Enter');

    const evt = await waitForEvent(page, 'wave.updated');
    expect((evt.data as { id: string; title: string }).title).toBe(newTitle);

    // Navigate back to the cove page via the sidebar entry.
    await page
      .locator('aside.side')
      .getByRole('button', { name: /WaveRenameCove/i })
      .click();
    await expect(page).toHaveURL(new RegExp(`/calm/cove/${cove.id}(\\?|$)`));

    // The wave row in the Cove page's `<section aria-label="Waves">`
    // is a real <button class="wave-row"> whose text content includes
    // the wave title. Scope to the row button (not its sibling
    // `.wave-row-delete` button, which also carries the title in
    // its aria-label and would trip strict-mode on a generic name
    // match — see WaveRow.tsx).
    await expect(
      page
        .getByRole('region', { name: 'Waves' })
        .locator('button.wave-row', { hasText: newTitle }),
    ).toBeVisible({ timeout: 8_000 });
    // The original title's row should NOT be visible — pin the
    // mutation cleanly so a regression that leaves both rows visible
    // (e.g. a stale duplicate cache entry) fails this test rather
    // than silently passing on the new row alone. Same row-class
    // scoping as above.
    await expect(
      page
        .getByRole('region', { name: 'Waves' })
        .locator('button.wave-row', { hasText: wave.title }),
    ).toHaveCount(0);
  });

  test('Wave rename: breadcrumb wave-title reflects new title (no remount needed)', async ({
    page,
    request,
  }) => {
    // The wave page's breadcrumb is `<cove>. · <wave title>`. The
    // existing "Wave rename" test asserts the post-rename re-locate
    // works — this test pins the more specific "the wave-title span in
    // the breadcrumb updates" surface in isolation, so a regression
    // that re-shows the input or shows the old title alongside the new
    // surfaces cleanly. The wave-page header is the same surface the
    // user types into; this exists primarily as parity with the cove
    // surfaces so the test matrix stays symmetric.
    const cove = await createUserCove(request, 'WaveCrumbCove');
    const wave = await createWaveInCove(request, cove.id, 'CrumbWaveOld');

    await page.goto(`/calm/wave/${wave.id}?trace=1`);
    await waitForCoveInSidebar(page, 'WaveCrumbCove');
    await clearEventTrace(page);

    const titleDisplay = page.getByRole('button', {
      name: wave.title,
      description: 'Rename wave',
    });
    await titleDisplay.click();
    const input = page.getByLabel('Wave title');
    await expect(input).toBeFocused();
    const newTitle = `CrumbWaveNew${Date.now()}`;
    await input.fill(newTitle);
    await page.keyboard.press('Enter');

    const evt = await waitForEvent(page, 'wave.updated');
    expect((evt.data as { id: string; title: string }).title).toBe(newTitle);

    // The breadcrumb wave-title span is the same DOM node that hosted
    // the input. After commit it returns as a span with the new title
    // and the cove crumb beside it.
    await expect(
      page.getByRole('button', { name: newTitle, description: 'Rename wave' }),
    ).toBeVisible({ timeout: 8_000 });
    // The cove crumb is unaffected by a wave rename; pin that the wave
    // rename did NOT clobber the cove name (e.g. via a wave.updated →
    // ['coves'] invalidation that returns stale data).
    await expect(
      page.locator('button.wave-cove', { hasText: 'WaveCrumbCove' }),
    ).toBeVisible();
  });

  test('Cove rename: Enter then Tab — no second PATCH with stale name (issue #288)', async ({
    page,
    request,
  }) => {
    // Regression for issue #288 — the "flash then revert" sidebar bug.
    //
    // Repro the exact keyboard sequence the user reports: focus the
    // rename input, type a new name, press Enter to commit, immediately
    // press Tab. Pre-fix this emitted TWO PATCHes — the first with the
    // NEW name (good) and a second with the OLD name (bad), because the
    // Enter `keyup` was delivered to the just-focused display button and
    // synthesized a click that re-entered edit mode with `draft` reset
    // to the (still pre-PATCH) `value`. The follow-up Tab then blurred
    // the re-mounted input and fired a second save() that PATCHed the
    // OLD name back to the kernel, which the WS-driven write-through
    // then propagated to the sidebar. The user saw the new name flash
    // on the sidebar (from the first PATCH's optimistic update + WS
    // event) and revert to the old name when the second PATCH landed.
    //
    // The fix sets a one-shot ref in save() when invoked via the
    // keyboard, and enter() consumes & ignores the next display-button
    // activation. We assert by counting PATCH requests and checking the
    // kernel ends up with the NEW name — not the OLD one.
    const cove = await createUserCove(request, 'EnterTabCove');
    await createWaveInCove(request, cove.id, 'EnterTabWave');

    await page.goto(`/calm/cove/${cove.id}?trace=1`);
    await waitForCoveInSidebar(page, 'EnterTabCove');
    await clearEventTrace(page);

    // Count every PATCH the page emits against this cove's REST row.
    // Use page.on so we capture both the (good) NEW-name PATCH and
    // any (bad) OLD-name PATCH that would slip through pre-fix.
    const patchBodies: string[] = [];
    page.on('request', (req) => {
      if (
        req.method() === 'PATCH' &&
        req.url().includes(`/api/coves/${cove.id}`) &&
        !req.url().includes('/waves')
      ) {
        patchBodies.push(req.postData() ?? '');
      }
    });

    const titleBtn = page.getByRole('button', {
      name: 'EnterTabCove',
      description: 'Rename cove name',
    });
    await expect(titleBtn).toBeVisible();
    await titleBtn.click();
    const input = page.getByLabel('Cove name');
    await expect(input).toBeFocused();

    const newName = `EnterTabRenamed${Date.now()}`;
    await input.selectText();
    await input.fill(newName);

    // The user-reported failure pattern: Enter to commit, then Tab.
    // Pressing them back-to-back is what races the Enter-keyup-click
    // against the input unmount.
    await page.keyboard.press('Enter');
    await page.keyboard.press('Tab');

    // Give the WS round-trip plus any racy second PATCH a window to
    // land before we assert single-PATCH. 1500ms is comfortably wider
    // than a normal kernel write + WS broadcast (the existing
    // wave-rename test uses the same window for `waitForEvent`).
    await page.waitForTimeout(1500);

    // Single-PATCH assertion runs FIRST so the negative case (pre-fix)
    // fails fast with a clear message rather than waiting on a sidebar
    // re-render that the second PATCH has thrown into flux.
    expect(
      patchBodies.length,
      `expected exactly 1 PATCH (one save), got ${patchBodies.length}: ${JSON.stringify(patchBodies)}`,
    ).toBe(1);
    expect(patchBodies[0]).toContain(newName);

    // Kernel must hold the NEW name — not have been rolled back.
    const listRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/coves`);
    const ours = ((await listRes.json()) as { id: string; name: string }[]).find(
      (c) => c.id === cove.id,
    );
    expect(ours?.name, 'kernel must hold the new name, not revert to old').toBe(newName);

    // Sidebar must show the new name (anchors the user-visible end of
    // the chain — this is the surface that flashed and reverted).
    await expect(
      page.locator('aside.side').getByRole('button', { name: new RegExp(newName, 'i') }),
    ).toBeVisible();
  });

  test('Wave delete dialog: Escape dismisses without deleting', async ({ page, request }) => {
    // Negative-path counterpart to the happy delete test above. The
    // ConfirmDialog primitive routes Esc to `onCancel` via the
    // underlying Dialog's `onClose` (see
    // `web/src/ui/ConfirmDialog/ConfirmDialog.tsx`); this asserts
    // that contract end-to-end:
    //   * Esc on the open dialog dismisses it,
    //   * no `wave.deleted` event lands on the trace within a
    //     bounded window (would mean the destructive action fired),
    //   * the wave row still exists per GET /api/waves/:id (200),
    //   * the page stays on the wave URL (no router push).
    const cove = await createUserCove(request, 'AtlasEsc');
    const wave = await createWaveInCove(request, cove.id, 'KeepMeEsc');

    await page.goto(`/calm/wave/${wave.id}?trace=1`);
    await waitForCoveInSidebar(page, 'AtlasEsc');
    await clearEventTrace(page);

    const deleteTrigger = page.getByRole('button', { name: `Delete wave "${wave.title}"` });
    await expect(deleteTrigger).toBeVisible();
    await deleteTrigger.click();

    const dialog = page.getByRole('dialog', { name: 'Delete wave?' });
    await expect(dialog).toBeVisible();
    // Cancel-safe focus invariant — same probe as the happy-path
    // test. If a future refactor lands focus on Confirm, this test
    // would still pass (Esc cancels regardless of focus position),
    // but the assertion documents the wider contract for the reader.
    await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();

    // Esc — Dialog handles the keydown and routes through `onClose
    // → onCancel`. The dialog unmounts; the destructive handler
    // never runs.
    await page.keyboard.press('Escape');
    await expect(dialog).not.toBeVisible();

    // Bounded negative event-trace assertion. Mirrors the pattern
    // used by the rename test above (`expect(async () =>
    // waitForEvent(…)).rejects.toThrow(/Timeout/)`). 1500ms is the
    // same window — comfortably wider than a normal kernel write +
    // WS broadcast roundtrip, narrow enough not to slow the suite.
    await expect(async () => {
      await waitForEvent(page, 'wave.deleted', 1500);
    }).rejects.toThrow(/Timeout/);

    // Wave row still present via REST.
    const detailRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/waves/${wave.id}`);
    expect(detailRes.status(), 'wave row must still exist after Esc-cancel').toBe(200);

    // URL didn't navigate away (the happy path would push to the
    // cove page; the cancel path must leave the router untouched).
    await expect(page).toHaveURL(new RegExp(`/calm/wave/${wave.id}(\\?|$)`));
  });

  test('Wave delete dialog: Cancel button click dismisses without deleting', async ({
    page,
    request,
  }) => {
    // Same cancel-without-deletion contract as the Esc test above,
    // but exercised through the *button click* path. ConfirmDialog
    // wires both into the same `onCancel` callback, so the
    // assertion shape is identical. We split the two paths into
    // separate tests so a regression that breaks one (e.g. a stray
    // `e.stopPropagation()` on the Cancel button) surfaces cleanly
    // without the other masking it.
    const cove = await createUserCove(request, 'AtlasCancel');
    const wave = await createWaveInCove(request, cove.id, 'KeepMeCancel');

    await page.goto(`/calm/wave/${wave.id}?trace=1`);
    await waitForCoveInSidebar(page, 'AtlasCancel');
    await clearEventTrace(page);

    const deleteTrigger = page.getByRole('button', { name: `Delete wave "${wave.title}"` });
    await expect(deleteTrigger).toBeVisible();
    await deleteTrigger.click();

    const dialog = page.getByRole('dialog', { name: 'Delete wave?' });
    await expect(dialog).toBeVisible();

    // Click Cancel — `onClick={onCancel}` on the Cancel button (see
    // `ConfirmDialog.tsx`). Dialog unmounts on the resulting state
    // change in the parent.
    const cancelBtn = dialog.getByRole('button', { name: 'Cancel' });
    await cancelBtn.click();
    await expect(dialog).not.toBeVisible();

    // No destructive event landed.
    await expect(async () => {
      await waitForEvent(page, 'wave.deleted', 1500);
    }).rejects.toThrow(/Timeout/);

    // Wave row still present.
    const detailRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/waves/${wave.id}`);
    expect(detailRes.status(), 'wave row must still exist after Cancel-click').toBe(200);

    // Still on the wave page.
    await expect(page).toHaveURL(new RegExp(`/calm/wave/${wave.id}(\\?|$)`));
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

  test('Cove delete cascades into MULTIPLE cove_folders rows', async ({ page, request }) => {
    // Stronger sibling of the single-claim cascade test above. The
    // migration 0015 `ON DELETE CASCADE` on `cove_folders.cove_id`
    // should drop EVERY row attached to the cove, not just one;
    // this test claims three non-overlapping paths and asserts that
    // each independently resolves to null post-delete.
    //
    // Why three rather than two: at the SQL layer CASCADE fans out
    // through the FK trigger, and a regression that accidentally
    // limits the cascade (e.g. by adding a `LIMIT 1` in a future
    // hand-rolled delete handler) would still pass with two rows
    // 50% of the time depending on insert order. Three claims push
    // the false-green probability low enough to catch the obvious
    // failure mode every run.
    //
    // We bypass `createWaveInCove` entirely (it auto-attaches a
    // `/tmp/playwright-cove-<id>` folder that would crowd the
    // assertion noise and could collide with the explicit claims
    // below if `Date.now()` falls in the wrong window). The cove
    // here exists purely as the parent of three folder claims.
    const cove = await createUserCove(request, 'AtlasMultiCascade');

    // Three non-overlapping paths. Per-run randomization (`Date.now`
    // + `Math.random`) guards against the unlikely case of the same
    // claim being live across a non-hermetic re-run; in the a11y
    // project the `beforeEach` reset already provides hermeticity
    // but the namespacing keeps the spec safe to re-read against a
    // shared server too.
    const ts = Date.now();
    const tag = Math.random().toString(36).slice(2, 8);
    const paths = [
      `/tmp/playwright-multi-${ts}-${tag}-alpha`,
      `/tmp/playwright-multi-${ts}-${tag}-bravo`,
      `/tmp/playwright-multi-${ts}-${tag}-charlie`,
    ];
    for (const p of paths) {
      const res = await request.post(
        `http://127.0.0.1:${REPLAY_PORT}/api/coves/${cove.id}/folders`,
        {
          data: { path: p },
          headers: { 'content-type': 'application/json' },
        },
      );
      expect(res.ok(), `claim ${p} → ${res.status()}`).toBe(true);
    }

    // Sanity: all three resolve to this cove BEFORE the cove
    // delete. Without this check, the post-delete null assertion
    // could false-green if the create-folder calls above silently
    // no-op'd somehow.
    for (const p of paths) {
      const res = await request.get(
        `http://127.0.0.1:${REPLAY_PORT}/api/coves/resolve?path=${encodeURIComponent(p)}`,
      );
      expect(res.ok()).toBe(true);
      const body = (await res.json()) as { cove_id: string } | null;
      expect(body, `resolve ${p} before delete`).not.toBeNull();
      expect(body!.cove_id).toBe(cove.id);
    }

    // Drive the delete through the UI confirm dialog so this test
    // covers the same path the user would. Mirrors the single-claim
    // test above.
    await page.goto(`/calm/cove/${cove.id}?trace=1`);
    await waitForCoveInSidebar(page, 'AtlasMultiCascade');
    await clearEventTrace(page);

    const deleteTrigger = page.getByRole('button', { name: `Delete cove "${cove.name}"` });
    await expect(deleteTrigger).toBeVisible();
    await deleteTrigger.click();

    const dialog = page.getByRole('dialog', { name: 'Delete cove?' });
    await expect(dialog).toBeVisible();
    const confirmBtn = dialog.getByRole('button', { name: 'Delete cove' });
    await confirmBtn.click();

    // Wait for the kernel to confirm the delete via the event bus
    // before probing the post-delete state — otherwise the resolve
    // calls below would race the FK CASCADE trigger.
    const evt = await waitForEvent(page, 'cove.deleted');
    expect((evt.data as { id: string }).id).toBe(cove.id);

    // The core assertion: every claimed path now resolves to null.
    // We probe each one independently rather than batching so a
    // partial-cascade regression (e.g. only the first row got
    // dropped) surfaces with a per-path failure message.
    for (const p of paths) {
      const res = await request.get(
        `http://127.0.0.1:${REPLAY_PORT}/api/coves/resolve?path=${encodeURIComponent(p)}`,
      );
      expect(res.ok()).toBe(true);
      const body = await res.json();
      expect(body, `cove_folders row for ${p} should CASCADE-drop`).toBeNull();
    }
  });
});
