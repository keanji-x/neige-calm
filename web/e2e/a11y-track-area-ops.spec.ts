// E2E coverage for issue #269 P2 — track + area mutation flows.
//
// Scope (per the PR2-of-#269 scope-agent brief):
//   * Track delete  — full UI confirm-dialog flow + assert kernel + event.
//   * Track rename  — extended beyond a11y-keyboard's F2/Enter happy
//                    path: Escape cancels, empty input is a no-op,
//                    mouse-click entry path also works.
//   * Area rename  — click / F2 to enter, Enter commits.
//   * Area delete  — confirm dialog → kernel row gone → cascade drops
//                    the `area_folders` row we claimed up front.
//
// Items explicitly NOT covered (and why):
//   * Track archive — the kernel exposes archive via
//     `PATCH /api/tracks/{id}` with `archived_at`, but the frontend
//     has no archive affordance today (verified by greping
//     `web/src/` for archive — only schema / type comments mention
//     it, no buttons or menu items). This PR is e2e-only and the
//     scope-agent brief forbids adding UI; tracked as a follow-up.
//
// Why a11y project (not chromium):
//   The chromium project targets the developer's `make dev` stack
//   at :4041 and is not a hermetic CI gate. a11y boots a fresh
//   in-memory replay binary per run (`_setup/replay-server.setup.ts`)
//   and exposes `POST /dev/reset` for per-test isolation. Putting
//   all five specs on a11y keeps the gate green deterministically.
//   The chromium-parity stand-in for the track-delete flow lives in
//   the `Track delete via confirm dialog` test below.
//
// Conventions inherited from `a11y-keyboard.spec.ts`:
//   * `?trace=1` enables `window.__neigeEvents__` so we can assert
//     against the event-trace ring buffer.
//   * Each test calls `resetReplayServer` + mints its own area so
//     state doesn't leak across planner files.
//   * The replay binary auto-bootstraps a hidden system area on
//     first Today render; we ignore it and anchor on user-minted
//     areas whose ids we capture from the REST response.

import { test, expect, type Page } from '@playwright/test';
import { createUserArea, createTrackInArea, resetReplayServer, REPLAY_PORT } from './helpers/reset';
import { clearEventTrace, waitForEvent } from './helpers/trace';

/** Wait for the WS-driven UI to mount our just-minted area in the
 *  sidebar (it travels via the `area.updated` event the REST mint
 *  fires). Mirrors the helper inside `a11y-keyboard.spec.ts` so each
 *  planner block can keep its bootstrap lockstep without importing
 *  across describe blocks. */
async function waitForAreaInSidebar(page: Page, name: string): Promise<void> {
  // `exact: true` excludes the per-row "Delete area \"<name>\"" button
  // whose accessible name also contains the area name (strict mode
  // otherwise resolves to two buttons).
  await expect(
    page.locator('aside.side').getByRole('button', { name, exact: true }),
  ).toBeVisible({ timeout: 15_000 });
  await page.waitForFunction(() => Array.isArray(window.__neigeEvents__));
}

/** Navigate by click — keyboard-only is the contract of
 *  `a11y-keyboard.spec.ts`, but this planner exercises mutation
 *  *flows*, not keyboard reachability. Clicks let us anchor on
 *  role+name without paying the `tabUntil` brittleness tax. */
async function gotoArea(page: Page, areaName: string): Promise<void> {
  // `exact: true` excludes the per-row "Delete area \"<name>\"" button
  // whose accessible name also contains areaName (strict mode otherwise
  // resolves to two buttons).
  await page
    .locator('aside.side')
    .getByRole('button', { name: areaName, exact: true })
    .click();
  await expect(page).toHaveURL(/\/calm\/area\/[^/]+(\?|$)/);
}

async function gotoTrackFromArea(page: Page, trackTitle: string): Promise<void> {
  // The TrackRow nav button's accessible name is `<title> Track
  // lifecycle: <Status>` (the lifecycle pill contributes aria text),
  // so an `exact: true` match on the bare title finds nothing. Anchor
  // a regex at the start of the title and require a trailing word
  // boundary — that pins the row's nav button while still excluding
  // the sibling `Delete "<title>"` button (whose name starts with
  // "Delete", not the title) and the `Pin track` button.
  await page
    .getByRole('region', { name: 'Tracks' })
    .getByRole('button', { name: new RegExp(`^${escapeRegExp(trackTitle)}\\b`) })
    .first()
    .click();
  await expect(page).toHaveURL(/\/calm\/track\/[^/]+(\?|$)/);
}

/** Escape regex metacharacters so a literal title (which may contain
 *  `.` from a Date.now() suffix, etc.) is matched verbatim. */
function escapeRegExp(literal: string): string {
  return literal.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

test.describe('a11y · track + area ops', () => {
  test.beforeEach(async ({ request }) => {
    // Hermetic per-test state: clear every accumulated row from the
    // shared replay kernel. See `helpers/reset.ts` for the rationale.
    await resetReplayServer(request);
  });

  test('Track delete via confirm dialog removes row, fires track.deleted, navigates back to area', async ({
    page,
    request,
  }) => {
    // ---- Setup: mint an area + track via REST so the page boots
    // with the rows already present (no need to drive the
    // sidebar's New area / New track UI — those flows have their
    // own coverage in track-create*.spec.ts).
    const area = await createUserArea(request, 'AtlasDel');
    const track = await createTrackInArea(request, area.id, 'TrackToDelete');

    await page.goto(`/calm/track/${track.id}?trace=1`);
    await waitForAreaInSidebar(page, 'AtlasDel');
    // Clear the bootstrap trace so the track.deleted assertion at the
    // end is unambiguous (the page-mount path emits its own
    // area.updated / track.updated as the WS feed drains).
    await clearEventTrace(page);

    // ---- Open the confirm dialog via the track header's × button.
    // `DeleteButton` (web/src/pages/_shared.tsx) renders an icon-
    // button whose aria-label is `Delete track "<title>"`; the
    // ConfirmDialog primitive then opens with title "Delete track?"
    // and default-focused Cancel.
    const deleteTrigger = page.getByRole('button', { name: `Delete track "${track.title}"` });
    await expect(deleteTrigger).toBeVisible();
    await deleteTrigger.click();

    const dialog = page.getByRole('dialog', { name: 'Delete track?' });
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
    const confirmBtn = dialog.getByRole('button', { name: 'Delete track' });
    await expect(confirmBtn).toBeFocused();
    await page.keyboard.press('Enter');

    // ---- Post-delete contract:
    //   * UI navigates back to the area page (router.tsx wires
    //     `go({name:'area', areaId:area.id})` on the track-page
    //     `onDeleteTrack` handler).
    //   * The track.deleted event fires on the trace buffer.
    //   * GET /api/tracks/<id> returns 404.
    await expect(page).toHaveURL(new RegExp(`/calm/area/${area.id}(\\?|$)`), { timeout: 10_000 });
    const evt = await waitForEvent(page, 'track.deleted');
    expect((evt.data as { id: string }).id).toBe(track.id);

    const detailRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/tracks/${track.id}`);
    expect(detailRes.status()).toBe(404);
  });

  test('Track rename: Escape cancels, empty input is a no-op, mouse-click enters edit mode', async ({
    page,
    request,
  }) => {
    const area = await createUserArea(request, 'AtlasRen');
    const track = await createTrackInArea(request, area.id, 'OriginalTitle');

    await page.goto(`/calm/track/${track.id}?trace=1`);
    await waitForAreaInSidebar(page, 'AtlasRen');
    await clearEventTrace(page);

    // The track title in `Track.tsx` renders as a
    // `<span role="button" aria-label={track.title} aria-describedby=…>`
    // when `onRenameTrack` is supplied. Locate by role + name +
    // description ("Rename track") so we don't collide with the
    // area-crumb button (same span tag, no description).
    const titleDisplay = page.getByRole('button', {
      name: track.title,
      description: 'Rename track',
    });
    await expect(titleDisplay).toBeVisible();

    // -------- Path 1: mouse-click enters edit mode --------
    // The span has `onClick={startRename}` when rename is wired;
    // proves the pointer-driven entry path that
    // a11y-keyboard.spec.ts (keyboard F2 only) doesn't cover.
    await titleDisplay.click();
    const input = page.getByLabel('Track title');
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
      `http://127.0.0.1:${REPLAY_PORT}/api/tracks/${track.id}`,
    );
    const detailBody = (await afterCancel.json()) as { track: { title: string } };
    expect(detailBody.track.title).toBe('OriginalTitle');

    // -------- Path 3: empty input is a no-op --------
    // `commitRename` in Track.tsx short-circuits when the trimmed
    // value is empty or unchanged — it sets editingTitle=false,
    // restores focus, and never calls onRenameTrack. We assert by
    // (a) the title staying the same in the DOM, (b) no
    // `track.updated` envelope landing on the trace within a
    // bounded window.
    await titleDisplay.click();
    await expect(input).toBeFocused();
    await input.fill('   '); // whitespace-only; trimmed == empty
    await page.keyboard.press('Enter');
    await expect(titleDisplay).toBeFocused();
    // Bounded wait — if a track.updated arrives within 1500ms the
    // empty-input branch is broken. We swallow the timeout (the
    // happy path) and re-throw on an unexpected event.
    await expect(async () => {
      await waitForEvent(page, 'track.updated', 1500);
    }).rejects.toThrow(/Timeout/);

    // -------- Path 4: real edit still commits --------
    // The earlier no-op paths must not have wedged any state;
    // a normal click-edit-Enter must still POST and update.
    await titleDisplay.click();
    await expect(input).toBeFocused();
    const newTitle = `Renamed${Date.now()}`;
    await input.fill(newTitle);
    await page.keyboard.press('Enter');
    const evt = await waitForEvent(page, 'track.updated');
    expect((evt.data as { id: string; title: string }).title).toBe(newTitle);
    // UI reflects the new title — re-locate, the old span unmounted.
    await expect(
      page.getByRole('button', { name: newTitle, description: 'Rename track' }),
    ).toBeVisible();
  });

  test('Area rename: click to edit, blur commits, GET /api/areas reflects new name', async ({
    page,
    request,
  }) => {
    const area = await createUserArea(request, 'OriginalArea');
    await createTrackInArea(request, area.id, 'Today');

    await page.goto(`/calm/area/${area.id}?trace=1`);
    await waitForAreaInSidebar(page, 'OriginalArea');
    await clearEventTrace(page);

    // `EditableTitle` in Area.tsx renders the area name as a real
    // <button class="h-display-rename"> with aria-label=value and
    // aria-describedby pointing at the "Rename area name" sr-only
    // hint. Locate via role + name + description for parity with
    // the track-rename planner above.
    const titleBtn = page.getByRole('button', {
      name: 'OriginalArea',
      description: 'Rename area name',
    });
    await expect(titleBtn).toBeVisible();
    await titleBtn.click();

    const input = page.getByLabel('Area name');
    await expect(input).toBeFocused();
    const newName = `RenamedArea${Date.now()}`;
    // Clear + fill via Playwright. The EditableTitle input mounts
    // with the current value pre-selected (microtask `select()` in
    // `enter()`), so we explicitly `selectText()` before `fill()` to
    // force a deterministic replace regardless of whether the
    // microtask landed before the test grabbed focus. We commit by
    // *blurring* the input (Tab-away) rather than pressing Enter —
    // this exercises the `onBlur={save}` commit path specifically.
    // The Enter-commit path is covered by the dedicated regression
    // test below ("Area rename: Enter then Tab — no second PATCH with
    // stale name (issue #288)"), which pins that the keyboard commit
    // doesn't leak a second PATCH via the Enter-keyup synthetic-click
    // race (issue #288, fixed in PR #292).
    await input.selectText();
    await input.fill(newName);
    // Blur by tabbing away — the input's `onBlur={save}` runs and
    // commits the rename. Tab moves focus to the next focusable
    // sibling (the delete-area icon button in the header), which
    // also proves the dialog's tab order didn't get wedged.
    await page.keyboard.press('Tab');

    // The kernel emits area.updated on rename — wait for it then
    // confirm the REST list reflects the new name.
    const evt = await waitForEvent(page, 'area.updated');
    expect((evt.data as { id: string; name: string }).name).toBe(newName);

    const listRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/areas`);
    expect(listRes.ok()).toBe(true);
    const areas = (await listRes.json()) as { id: string; name: string }[];
    const ours = areas.find((c) => c.id === area.id);
    expect(ours, 'area row still present after rename').toBeDefined();
    expect(ours?.name).toBe(newName);

    // The header re-renders with the new name; the rename button's
    // accessible name is now the new value.
    await expect(
      page.getByRole('button', { name: newName, description: 'Rename area name' }),
    ).toBeVisible();
  });

  // -----------------------------------------------------------------------
  // Rename UI-surface propagation tests.
  //
  // The block below pins that an area / track rename propagates to *every*
  // UI surface that names the entity — not just the kernel row + the page
  // header (which the existing tests above already cover). New surfaces:
  //
  //   * Sidebar area entry  (the user-reported bug surface — see #288.
  //                          Passes here; see the test's own comment for
  //                          why no `test.fail()` annotation is used.)
  //   * Area page track list (rename on the track detail page → the row in
  //                          the Area page reflects the new name)
  //   * Track breadcrumb back-link to area (rename the area → the track
  //                          page's area crumb updates)
  //   * Area-list cache invalidation after nav-away/nav-back
  //
  // Each test resets state in the planner-level beforeEach and mints its
  // own area + track so the assertions don't share fate with prior tests.
  // -----------------------------------------------------------------------

  test('Area rename: sidebar entry reflects new name', async ({ page, request }) => {
    // Issue #288 — user reported that after renaming an area via the
    // area-header inline rename, the sidebar entry still shows the
    // OLD name even though `area.updated` fires and REST `GET
    // /api/areas` reflects the new name. The kernel half is covered
    // by the existing "click to edit, blur commits, GET /api/areas
    // reflects new name" test above; this test pins the UI half
    // specifically — the sidebar's area-nav button text.
    //
    // In the hermetic a11y replay environment this assertion CURRENTLY
    // passes (the WebSocket → eventBridge → React-Query invalidation
    // path runs end-to-end on every event). The user's report came
    // against the production bundle; if a regression slips the
    // invalidation off the `area.updated` arm — or if a future memo
    // anywhere on the `useAreasQuery` → Sidebar render path traps the
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
    // Multi-area preamble: mint two siblings so the sidebar renders a
    // proper list. The user's report came against a workspace with
    // multiple areas; a single-area sidebar is a degenerate case that
    // some failure modes (e.g. memoized-by-length list) could miss.
    await createUserArea(request, 'OtherAreaA');
    const area = await createUserArea(request, 'SidebarStaleArea');
    await createUserArea(request, 'OtherAreaB');
    await createTrackInArea(request, area.id, 'Today');

    // Land on Today first then navigate into the area via the sidebar
    // — mirrors the real user flow (the reporter doesn't deep-link to
    // an area URL, they click the sidebar entry). If the bug is
    // navigation-history sensitive (e.g. a stale memo retained across
    // the route boundary), the deep-link path would mask it.
    await page.goto('/calm/?trace=1');
    await waitForAreaInSidebar(page, 'SidebarStaleArea');
    await gotoArea(page, 'SidebarStaleArea');
    await clearEventTrace(page);

    // Drive the rename through the same UI path as the existing
    // "click → blur commits" test above — this is the surface the
    // user reported the bug against.
    const titleBtn = page.getByRole('button', {
      name: 'SidebarStaleArea',
      description: 'Rename area name',
    });
    await titleBtn.click();

    const input = page.getByLabel('Area name');
    await expect(input).toBeFocused();
    const newName = `SidebarFreshArea${Date.now()}`;
    await input.selectText();
    await input.fill(newName);
    // Blur commits via the same `onBlur={save}` code path as Enter; we
    // tab away to mimic the user's natural mouse-driven workflow.
    await page.keyboard.press('Tab');

    // Wait for the kernel to confirm the rename landed. If this times
    // out the bug we're pinning has shifted shape (kernel-side
    // regression) and the test will fail for the wrong reason — make
    // the failure mode loud rather than silently flaky.
    const evt = await waitForEvent(page, 'area.updated');
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
    // area.updated event has already landed so this is purely about
    // the React-Query → Sidebar render path. If the production-only
    // bug ever surfaces in this harness (see #288), this assertion
    // is what turns red.
    await expect(
      page.locator('aside.side').getByRole('button', { name: 'SidebarStaleArea', exact: true }),
      'old area name must disappear from sidebar after rename',
    ).toHaveCount(0, { timeout: 1_500 });
    await expect(
      page.locator('aside.side').getByRole('button', { name: newName, exact: true }),
      'new area name must appear in sidebar after rename',
    ).toBeVisible({ timeout: 1_500 });
  });

  test('Area rename: track page breadcrumb reflects new name', async ({ page, request }) => {
    // The track page's header carries a "back to area" breadcrumb
    // (`<button class="track-area">`) that displays the area name. When
    // the user renames the area from the area page, then navigates to
    // a track, the breadcrumb should display the new name (covered by
    // the React-Query invalidation on `area.updated`). This test
    // exercises the linked-surface case: rename the area from its own
    // header, navigate into a track, assert the crumb shows the new name.
    //
    // #290 — pre-#290 the test had to land on Today first then click
    // the sidebar to avoid a race between `/dev/reset` (which resets
    // `sqlite_sequence`, so re-seeded events restart at id=1) and the
    // WS client's persisted cursor from prior tests. The client now
    // detects that mismatch via `_replay_complete._id < lastEventId`
    // (see `web/src/api/events.ts`) and re-bootstraps the cache, so
    // we can deep-link directly to the area URL again. See
    // `a11y-deep-link-after-reset.spec.ts` for the dedicated test of
    // the deep-link-after-reset contract.
    const area = await createUserArea(request, 'CrumbAreaOld');
    const track = await createTrackInArea(request, area.id, 'WorkTrack');

    await page.goto(`/calm/area/${area.id}?trace=1`);
    await waitForAreaInSidebar(page, 'CrumbAreaOld');
    await clearEventTrace(page);

    // Rename via the same flow as the existing area-rename test.
    const titleBtn = page.getByRole('button', {
      name: 'CrumbAreaOld',
      description: 'Rename area name',
    });
    await titleBtn.click();
    const input = page.getByLabel('Area name');
    await expect(input).toBeFocused();
    const newName = `CrumbAreaNew${Date.now()}`;
    await input.selectText();
    await input.fill(newName);
    await page.keyboard.press('Tab');

    // Wait for the kernel confirmation before navigating — otherwise
    // the track page would race the rename request.
    const evt = await waitForEvent(page, 'area.updated');
    expect((evt.data as { id: string; name: string }).name).toBe(newName);

    // The Tab keypress commits the rename AND moves focus to the next
    // header control, but the `EditableTitle` input doesn't leave edit
    // mode until its `onBlur={save}` settles and the header re-renders
    // in display mode. Wait for that transition (the rename button
    // carrying the NEW name) before clicking into the track list — an
    // immediate click can otherwise land while the still-open editor
    // overlays the header, leaving `gotoTrackFromArea` waiting on a
    // track row click that never registers.
    await expect(
      page.getByRole('button', { name: newName, description: 'Rename area name' }),
    ).toBeVisible({ timeout: 8_000 });

    // Navigate to the track detail via the area-page track list (mirrors
    // user click flow + avoids the cold goto's lazy-TrackGrid compile
    // hit that pushes a fresh `page.goto` past Playwright's default
    // 30s timeout in this hermetic Vite-on-cargo stack).
    await gotoTrackFromArea(page, track.title);

    // Find the breadcrumb back-link button. It has the new area name
    // as its accessible name (text content). Scope by class to
    // disambiguate from any other "<area name>" button on the page.
    await expect(
      page.locator('button.track-area', { hasText: newName }),
    ).toBeVisible({ timeout: 8_000 });
  });

  test('Area rename: nav away + back, sidebar shows new name (no stale cache)', async ({
    page,
    request,
  }) => {
    // Cache-invalidation flavor of the surface test. Even if the
    // immediate `area.updated` → sidebar path were broken, a navigation
    // round-trip should at minimum trigger a refetch of `['areas']`
    // when the user lands back on a route that depends on it. Today's
    // route reads the same area query, so a Today → Area → rename →
    // Today round trip should leave the sidebar fresh.
    //
    // This overlaps with the sidebar-rename test above but adds the
    // route-boundary dimension: a refetch-on-area.updated regression
    // and a re-render-on-route-change regression would each show up
    // differently across these two tests.
    const area = await createUserArea(request, 'NavBackAreaOld');
    await createTrackInArea(request, area.id, 'Today');

    await page.goto(`/calm/area/${area.id}?trace=1`);
    await waitForAreaInSidebar(page, 'NavBackAreaOld');
    await clearEventTrace(page);

    const titleBtn = page.getByRole('button', {
      name: 'NavBackAreaOld',
      description: 'Rename area name',
    });
    await titleBtn.click();
    const input = page.getByLabel('Area name');
    await expect(input).toBeFocused();
    const newName = `NavBackAreaNew${Date.now()}`;
    await input.selectText();
    await input.fill(newName);
    await page.keyboard.press('Tab');

    const evt = await waitForEvent(page, 'area.updated');
    expect((evt.data as { id: string; name: string }).name).toBe(newName);

    // Navigate to Today and back. The Sidebar's Today button is the
    // only Sidebar-navigation 'nav' with that name.
    await page
      .locator('aside.side')
      .getByRole('button', { name: 'Today', exact: true })
      .click();
    await expect(page).toHaveURL(/\/calm\/?(\?|$)/);

    // Return to the area. The sidebar entry should now reflect the
    // new name — either the area.updated path repaired itself across
    // route changes, or the route boundary forced a re-render.
    await expect(
      page.locator('aside.side').getByRole('button', { name: newName, exact: true }),
    ).toBeVisible({ timeout: 8_000 });
  });

  test('Track rename: track row in area page reflects new title', async ({ page, request }) => {
    // The Area page's Tracks list (rendered inside <section
    // aria-label="Tracks">) is a track-rename surface that the existing
    // track-rename test doesn't cover — it only asserts the header.
    // Rename the track from its detail page, navigate back to the area
    // page, and assert the row's accessible name reflects the new
    // title.
    const area = await createUserArea(request, 'TrackRenameArea');
    const track = await createTrackInArea(request, area.id, 'TrackOriginalTitle');

    // #290 — pre-#290 the test had to land on Today first then click
    // the sidebar; deep-link is safe again now that the client detects
    // server resets via `_replay_complete._id < lastEventId` and
    // re-bootstraps. See `web/src/api/events.ts` + the dedicated
    // `a11y-deep-link-after-reset.spec.ts`.
    await page.goto(`/calm/track/${track.id}?trace=1`);
    await waitForAreaInSidebar(page, 'TrackRenameArea');
    await clearEventTrace(page);

    // Drive the rename through the track-header inline edit, mirroring
    // the existing "Track rename" test above.
    const titleDisplay = page.getByRole('button', {
      name: track.title,
      description: 'Rename track',
    });
    await titleDisplay.click();
    const input = page.getByLabel('Track title');
    await expect(input).toBeFocused();
    const newTitle = `TrackRenamed${Date.now()}`;
    await input.fill(newTitle);
    await page.keyboard.press('Enter');

    const evt = await waitForEvent(page, 'track.updated');
    expect((evt.data as { id: string; title: string }).title).toBe(newTitle);

    // Navigate back to the area page via the sidebar entry.
    await page
      .locator('aside.side')
      .getByRole('button', { name: 'TrackRenameArea', exact: true })
      .click();
    await expect(page).toHaveURL(new RegExp(`/calm/area/${area.id}(\\?|$)`));

    // The track row in the Area page's `<section aria-label="Tracks">`
    // is a real <button class="track-row"> whose text content includes
    // the track title. Scope to the row button (not its sibling
    // `.track-row-delete` button, which also carries the title in
    // its aria-label and would trip strict-mode on a generic name
    // match — see TrackRow.tsx).
    await expect(
      page
        .getByRole('region', { name: 'Tracks' })
        .locator('button.track-row', { hasText: newTitle }),
    ).toBeVisible({ timeout: 8_000 });
    // The original title's row should NOT be visible — pin the
    // mutation cleanly so a regression that leaves both rows visible
    // (e.g. a stale duplicate cache entry) fails this test rather
    // than silently passing on the new row alone. Same row-class
    // scoping as above.
    await expect(
      page
        .getByRole('region', { name: 'Tracks' })
        .locator('button.track-row', { hasText: track.title }),
    ).toHaveCount(0);
  });

  test('Track rename: breadcrumb track-title reflects new title (no remount needed)', async ({
    page,
    request,
  }) => {
    // The track page's breadcrumb is `<area>. · <track title>`. The
    // existing "Track rename" test asserts the post-rename re-locate
    // works — this test pins the more specific "the track-title span in
    // the breadcrumb updates" surface in isolation, so a regression
    // that re-shows the input or shows the old title alongside the new
    // surfaces cleanly. The track-page header is the same surface the
    // user types into; this exists primarily as parity with the area
    // surfaces so the test matrix stays symmetric.
    const area = await createUserArea(request, 'TrackCrumbArea');
    const track = await createTrackInArea(request, area.id, 'CrumbTrackOld');

    await page.goto(`/calm/track/${track.id}?trace=1`);
    await waitForAreaInSidebar(page, 'TrackCrumbArea');
    await clearEventTrace(page);

    const titleDisplay = page.getByRole('button', {
      name: track.title,
      description: 'Rename track',
    });
    await titleDisplay.click();
    const input = page.getByLabel('Track title');
    await expect(input).toBeFocused();
    const newTitle = `CrumbTrackNew${Date.now()}`;
    await input.fill(newTitle);
    await page.keyboard.press('Enter');

    const evt = await waitForEvent(page, 'track.updated');
    expect((evt.data as { id: string; title: string }).title).toBe(newTitle);

    // The breadcrumb track-title span is the same DOM node that hosted
    // the input. After commit it returns as a span with the new title
    // and the area crumb beside it.
    await expect(
      page.getByRole('button', { name: newTitle, description: 'Rename track' }),
    ).toBeVisible({ timeout: 8_000 });
    // The area crumb is unaffected by a track rename; pin that the track
    // rename did NOT clobber the area name (e.g. via a track.updated →
    // ['areas'] invalidation that returns stale data).
    await expect(
      page.locator('button.track-area', { hasText: 'TrackCrumbArea' }),
    ).toBeVisible();
  });

  test('Area rename: Enter then Tab — no second PATCH with stale name (issue #288)', async ({
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
    const area = await createUserArea(request, 'EnterTabArea');
    await createTrackInArea(request, area.id, 'EnterTabTrack');

    await page.goto(`/calm/area/${area.id}?trace=1`);
    await waitForAreaInSidebar(page, 'EnterTabArea');
    await clearEventTrace(page);

    // Count every PATCH the page emits against this area's REST row.
    // Use page.on so we capture both the (good) NEW-name PATCH and
    // any (bad) OLD-name PATCH that would slip through pre-fix.
    const patchBodies: string[] = [];
    page.on('request', (req) => {
      if (
        req.method() === 'PATCH' &&
        req.url().includes(`/api/areas/${area.id}`) &&
        !req.url().includes('/tracks')
      ) {
        patchBodies.push(req.postData() ?? '');
      }
    });

    const titleBtn = page.getByRole('button', {
      name: 'EnterTabArea',
      description: 'Rename area name',
    });
    await expect(titleBtn).toBeVisible();
    await titleBtn.click();
    const input = page.getByLabel('Area name');
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
    // track-rename test uses the same window for `waitForEvent`).
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
    const listRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/areas`);
    const ours = ((await listRes.json()) as { id: string; name: string }[]).find(
      (c) => c.id === area.id,
    );
    expect(ours?.name, 'kernel must hold the new name, not revert to old').toBe(newName);

    // Sidebar must show the new name (anchors the user-visible end of
    // the chain — this is the surface that flashed and reverted).
    await expect(
      page.locator('aside.side').getByRole('button', { name: newName, exact: true }),
    ).toBeVisible();
  });

  test('Track delete dialog: Escape dismisses without deleting', async ({ page, request }) => {
    // Negative-path counterpart to the happy delete test above. The
    // ConfirmDialog primitive routes Esc to `onCancel` via the
    // underlying Dialog's `onClose` (see
    // `web/src/ui/ConfirmDialog/ConfirmDialog.tsx`); this asserts
    // that contract end-to-end:
    //   * Esc on the open dialog dismisses it,
    //   * no `track.deleted` event lands on the trace within a
    //     bounded window (would mean the destructive action fired),
    //   * the track row still exists per GET /api/tracks/:id (200),
    //   * the page stays on the track URL (no router push).
    const area = await createUserArea(request, 'AtlasEsc');
    const track = await createTrackInArea(request, area.id, 'KeepMeEsc');

    await page.goto(`/calm/track/${track.id}?trace=1`);
    await waitForAreaInSidebar(page, 'AtlasEsc');
    await clearEventTrace(page);

    const deleteTrigger = page.getByRole('button', { name: `Delete track "${track.title}"` });
    await expect(deleteTrigger).toBeVisible();
    await deleteTrigger.click();

    const dialog = page.getByRole('dialog', { name: 'Delete track?' });
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
      await waitForEvent(page, 'track.deleted', 1500);
    }).rejects.toThrow(/Timeout/);

    // Track row still present via REST.
    const detailRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/tracks/${track.id}`);
    expect(detailRes.status(), 'track row must still exist after Esc-cancel').toBe(200);

    // URL didn't navigate away (the happy path would push to the
    // area page; the cancel path must leave the router untouched).
    await expect(page).toHaveURL(new RegExp(`/calm/track/${track.id}(\\?|$)`));
  });

  test('Track delete dialog: Cancel button click dismisses without deleting', async ({
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
    const area = await createUserArea(request, 'AtlasCancel');
    const track = await createTrackInArea(request, area.id, 'KeepMeCancel');

    await page.goto(`/calm/track/${track.id}?trace=1`);
    await waitForAreaInSidebar(page, 'AtlasCancel');
    await clearEventTrace(page);

    const deleteTrigger = page.getByRole('button', { name: `Delete track "${track.title}"` });
    await expect(deleteTrigger).toBeVisible();
    await deleteTrigger.click();

    const dialog = page.getByRole('dialog', { name: 'Delete track?' });
    await expect(dialog).toBeVisible();

    // Click Cancel — `onClick={onCancel}` on the Cancel button (see
    // `ConfirmDialog.tsx`). Dialog unmounts on the resulting state
    // change in the parent.
    const cancelBtn = dialog.getByRole('button', { name: 'Cancel' });
    await cancelBtn.click();
    await expect(dialog).not.toBeVisible();

    // No destructive event landed.
    await expect(async () => {
      await waitForEvent(page, 'track.deleted', 1500);
    }).rejects.toThrow(/Timeout/);

    // Track row still present.
    const detailRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/tracks/${track.id}`);
    expect(detailRes.status(), 'track row must still exist after Cancel-click').toBe(200);

    // Still on the track page.
    await expect(page).toHaveURL(new RegExp(`/calm/track/${track.id}(\\?|$)`));
  });

  test('Area delete via confirm dialog cascades into area_folders', async ({
    page,
    request,
  }) => {
    const area = await createUserArea(request, 'AtlasCascade');
    await createTrackInArea(request, area.id, 'Today');

    // Claim a folder up front so we have something to verify
    // cascade-deleted. (Pre-#1147-S3 `createTrackInArea` also attached
    // an invented `/tmp/playwright-area-<id>` folder; it now sends no
    // cwd, so this explicit claim is the area's only folder row —
    // which only sharpens the assertion below: "the folder I claimed
    // for this test is gone", never "some folder is gone".)
    //
    // `POST /api/areas/:id/folders` carries no filesystem contract —
    // a folder claim is a naming/ownership record, not an attached
    // workspace — so this path does not need to exist on disk.
    const folderPath = `/tmp/playwright-cascade-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const folderRes = await request.post(
      `http://127.0.0.1:${REPLAY_PORT}/api/areas/${area.id}/folders`,
      {
        data: { path: folderPath },
        headers: { 'content-type': 'application/json' },
      },
    );
    expect(folderRes.ok(), `area folder POST: ${folderRes.status()}`).toBe(true);
    const folder = (await folderRes.json()) as { id: number; area_id: string; path: string };
    expect(folder.area_id).toBe(area.id);

    // Sanity check: the resolve endpoint finds the folder we just
    // claimed before we drop the area.
    const resolveBefore = await request.get(
      `http://127.0.0.1:${REPLAY_PORT}/api/areas/resolve?path=${encodeURIComponent(folderPath)}`,
    );
    expect(resolveBefore.ok()).toBe(true);
    const resolvedBefore = (await resolveBefore.json()) as { area_id: string } | null;
    expect(resolvedBefore?.area_id).toBe(area.id);

    await page.goto(`/calm/area/${area.id}?trace=1`);
    await waitForAreaInSidebar(page, 'AtlasCascade');
    await clearEventTrace(page);

    // AreaPage header carries `<DeleteButton label={Delete area "<name>"}>`
    // — see Area.tsx. Open the dialog, confirm.
    const deleteTrigger = page.getByRole('button', { name: `Delete area "${area.name}"` });
    await expect(deleteTrigger).toBeVisible();
    await deleteTrigger.click();

    const dialog = page.getByRole('dialog', { name: 'Delete area?' });
    await expect(dialog).toBeVisible();
    const confirmBtn = dialog.getByRole('button', { name: 'Delete area' });
    await confirmBtn.click();

    // ---- Post-delete contract:
    //   * area.deleted event fires.
    //   * The area no longer appears in GET /api/areas.
    //   * The area_folders row CASCADE-dropped (migration 0015):
    //     /api/areas/resolve returns null for the same path.
    //   * Router navigates back to the Today page (router.tsx wires
    //     `go({name:'today'})` on the area-page delete handler →
    //     the indexRoute at `/`, which under `basepath: '/calm'`
    //     shows in the browser URL as `/calm/` — no `/today` suffix).
    const evt = await waitForEvent(page, 'area.deleted');
    expect((evt.data as { id: string }).id).toBe(area.id);

    await expect(page).toHaveURL(/\/calm\/(\?|$)/, { timeout: 10_000 });

    const listRes = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/areas`);
    const areas = (await listRes.json()) as { id: string }[];
    expect(areas.find((c) => c.id === area.id), 'area row should be gone').toBeUndefined();

    const resolveAfter = await request.get(
      `http://127.0.0.1:${REPLAY_PORT}/api/areas/resolve?path=${encodeURIComponent(folderPath)}`,
    );
    expect(resolveAfter.ok()).toBe(true);
    const resolvedAfter = await resolveAfter.json();
    expect(resolvedAfter, 'area_folders row should CASCADE-drop with the area').toBeNull();
  });

  test('Area delete cascades into MULTIPLE area_folders rows', async ({ page, request }) => {
    // Stronger sibling of the single-claim cascade test above. The
    // migration 0015 `ON DELETE CASCADE` on `area_folders.area_id`
    // should drop EVERY row attached to the area, not just one;
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
    // We bypass `createTrackInArea` entirely — the area here exists
    // purely as the parent of three folder claims, so a track would be
    // pure noise. (Pre-#1147-S3 the helper also auto-attached a
    // `/tmp/playwright-area-<id>` folder that could have collided with
    // the explicit claims below; it sends no cwd now, but bypassing it
    // still keeps this area's folder set exactly the three paths the
    // assertions name.)
    const area = await createUserArea(request, 'AtlasMultiCascade');

    // Three non-overlapping paths. Per-run randomization (`Date.now`
    // + `Math.random`) guards against the unlikely case of the same
    // claim being live across a non-hermetic re-run; in the a11y
    // project the `beforeEach` reset already provides hermeticity
    // but the namespacing keeps the planner safe to re-read against a
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
        `http://127.0.0.1:${REPLAY_PORT}/api/areas/${area.id}/folders`,
        {
          data: { path: p },
          headers: { 'content-type': 'application/json' },
        },
      );
      expect(res.ok(), `claim ${p} → ${res.status()}`).toBe(true);
    }

    // Sanity: all three resolve to this area BEFORE the area
    // delete. Without this check, the post-delete null assertion
    // could false-green if the create-folder calls above silently
    // no-op'd somehow.
    for (const p of paths) {
      const res = await request.get(
        `http://127.0.0.1:${REPLAY_PORT}/api/areas/resolve?path=${encodeURIComponent(p)}`,
      );
      expect(res.ok()).toBe(true);
      const body = (await res.json()) as { area_id: string } | null;
      expect(body, `resolve ${p} before delete`).not.toBeNull();
      expect(body!.area_id).toBe(area.id);
    }

    // Drive the delete through the UI confirm dialog so this test
    // covers the same path the user would. Mirrors the single-claim
    // test above.
    await page.goto(`/calm/area/${area.id}?trace=1`);
    await waitForAreaInSidebar(page, 'AtlasMultiCascade');
    await clearEventTrace(page);

    const deleteTrigger = page.getByRole('button', { name: `Delete area "${area.name}"` });
    await expect(deleteTrigger).toBeVisible();
    await deleteTrigger.click();

    const dialog = page.getByRole('dialog', { name: 'Delete area?' });
    await expect(dialog).toBeVisible();
    const confirmBtn = dialog.getByRole('button', { name: 'Delete area' });
    await confirmBtn.click();

    // Wait for the kernel to confirm the delete via the event bus
    // before probing the post-delete state — otherwise the resolve
    // calls below would race the FK CASCADE trigger.
    const evt = await waitForEvent(page, 'area.deleted');
    expect((evt.data as { id: string }).id).toBe(area.id);

    // The core assertion: every claimed path now resolves to null.
    // We probe each one independently rather than batching so a
    // partial-cascade regression (e.g. only the first row got
    // dropped) surfaces with a per-path failure message.
    for (const p of paths) {
      const res = await request.get(
        `http://127.0.0.1:${REPLAY_PORT}/api/areas/resolve?path=${encodeURIComponent(p)}`,
      );
      expect(res.ok()).toBe(true);
      const body = await res.json();
      expect(body, `area_folders row for ${p} should CASCADE-drop`).toBeNull();
    }
  });
});
