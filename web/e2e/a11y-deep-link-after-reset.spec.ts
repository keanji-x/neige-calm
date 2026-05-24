// E2E coverage for issue #290 — deep-link directly to an entity URL
// immediately after `resetReplayServer()` must not race the WS resync.
//
// Pre-#290 reproduction: the WS client at `web/src/api/events.ts`
// persists `lastEventId` to `localStorage['calm:sync:cursor']`. After
// `POST /dev/reset` wipes `sqlite_sequence` (so re-seeded events
// restart at id=1), a fresh page load reads the stale cursor (e.g.
// id=42 from a prior test) and subscribes with `since=42`. The server
// replies with `_replay_complete` whose `_id` is the actual log tip —
// post-reset that tip is small (under the client's cursor) — and the
// new write IDs (1, 2, 3, ...) coming in over the same socket get
// dropped by the `advanceCursor` no-regress guard, so the cache never
// fills.
//
// Symptom in the test harness: deep-linking to `/calm/cove/<id>?trace=1`
// directly after reset would hang on "Connecting to calm-server…" or
// render a stale React-Query cache. The workaround was to land on
// Today first then navigate via the sidebar (the Today route's
// initial-data queries are tolerant of the WS-resync window).
//
// Fix (#290): the server now stamps `_replay_complete._id` with the
// log tip (`MAX(events.id)`) not the in-window high-water mark, and
// the client treats `_id < lastEventId` as a reset signal — it clears
// the cursor + fires the snapshot listeners (which the bridge handles
// by `qc.clear()`ing) and bounces the socket so the next reconnect
// comes up cold. Direct deep-link works again.
//
// This test pins the deep-link-after-reset contract: it would have
// failed pre-#290 (the cove name never renders within the timeout), and
// passes with the fix in place.

import { test, expect } from '@playwright/test';
import { createUserCove, createWaveInCove, resetReplayServer } from './helpers/reset';
import { waitForEvent } from './helpers/trace';

test.describe('a11y · deep-link after reset (issue #290)', () => {
  test.beforeEach(async ({ request }) => {
    await resetReplayServer(request);
  });

  test('Deep-link to cove directly after resetReplayServer() renders cove name', async ({
    page,
    request,
  }) => {
    // Mint cove + wave via REST so the page boots with real rows on
    // the kernel side; the bug we're pinning is purely about the WS
    // sync engine dropping events the kernel already persisted.
    const cove = await createUserCove(request, 'DeepLinkCove');
    await createWaveInCove(request, cove.id, 'AnchorWave');

    // Critically: NO `page.goto('/calm/?trace=1')` first. Deep-link
    // straight to the cove URL — this is the path that races the
    // WS resync pre-#290.
    await page.goto(`/calm/cove/${cove.id}?trace=1`);

    // The cove header renders the cove name as a button with
    // `aria-label=<name>` and `aria-describedby` pointing at the
    // "Rename cove name" sr-only hint (see `Cove.tsx`). Use the same
    // role+name+description locator the rename tests use so this
    // test's assertion lines up with the canonical cove-name surface.
    // 15s timeout matches `waitForCoveInSidebar` — long enough for a
    // fresh WS connect + bridge mount + initial data refetch, narrow
    // enough that the pre-#290 hang surfaces as a failure.
    await expect(
      page.getByRole('button', { name: 'DeepLinkCove', description: 'Rename cove name' }),
    ).toBeVisible({ timeout: 15_000 });

    // Sidebar entry should also be live — pin the cross-surface
    // contract so a regression that only fixes the page-local query
    // (but not the WS-driven sidebar cache) still trips the test.
    await expect(
      page.locator('aside.side').getByRole('button', { name: /DeepLinkCove/i }),
    ).toBeVisible({ timeout: 5_000 });
  });

  test('Deep-link to wave directly after resetReplayServer() renders wave title', async ({
    page,
    request,
  }) => {
    // Parallel of the cove-deep-link case for the wave URL. The wave
    // page's initial-data query goes through `useWaveDetailQuery` —
    // same WS-resync race surface, same pre-#290 hang risk.
    const cove = await createUserCove(request, 'DeepLinkWaveCove');
    const wave = await createWaveInCove(request, cove.id, 'DeepLinkWave');

    await page.goto(`/calm/wave/${wave.id}?trace=1`);

    // Wave title locator mirrors the wave-rename test's selector
    // (role=button + name=title + description="Rename wave"). Same
    // 15s budget for the same reason.
    await expect(
      page.getByRole('button', { name: wave.title, description: 'Rename wave' }),
    ).toBeVisible({ timeout: 15_000 });

    // Cross-surface: the wave page's `<button class="wave-cove">`
    // breadcrumb back-link carries the cove name. Pre-#290 the
    // breadcrumb would render stale (or empty) when the WS-resync
    // race lost a `cove.updated` event.
    await expect(page.locator('button.wave-cove', { hasText: 'DeepLinkWaveCove' })).toBeVisible({
      timeout: 5_000,
    });
  });

  test('Stale cursor in localStorage triggers reset and the bus re-bootstraps', async ({
    page,
    request,
  }) => {
    // Direct reproduction of the pre-#290 failure mode. We:
    //
    //   1. Pre-poison `localStorage['calm:sync:cursor']` with a huge
    //      id far above anything the post-reset log can hold — same
    //      effect as a prior test's leftover cursor would have.
    //   2. Open Today so the bridge mounts and the WS connects.
    //   3. Assert the bus delivered `cove.updated` to the trace ring
    //      buffer AFTER the reset bounce — pre-#290 the WS would
    //      stick on the stale cursor and the bus would drop every
    //      post-reset event.
    //
    // The `localStorage.setItem` call has to run BEFORE the app's
    // bundle loads, so we install an init script that fires on every
    // page open in this test scope. Playwright runs `addInitScript`
    // before any author scripts on the page — perfect for seeding
    // browser storage that the app reads at boot.
    //
    // We land on Today (not the deep-link entity URL) because the
    // persisted React-Query / IndexedDB cache layer can mask a
    // route-local refetch behind a stale snapshot in the brief
    // window between `qc.clear()` and the WS reconnect's fresh
    // replay — that's a separate cache-paint surface from the
    // bus-resync contract this test pins. The deep-link contract is
    // covered by the two tests above; this test specifically pins
    // "the WS bus recovers from a stale cursor."
    const cove = await createUserCove(request, 'StaleCursorCove');
    await createWaveInCove(request, cove.id, 'AnchorWave');

    await page.addInitScript(() => {
      // 999999 is well past any id the post-reset replay binary will
      // ever produce in a single test run (fixture has < 30 events,
      // plus a handful of REST writes per test).
      localStorage.setItem('calm:sync:cursor', '999999');
    });

    await page.goto('/calm/?trace=1');

    // The reset-detection path fires `_snapshot_required` listeners,
    // bounces the socket, and reconnects cold under `since=0`. The
    // trace ring buffer is set up under `?trace=1`; after the bounce
    // the second WS pass delivers a fresh replay, including
    // `cove.updated` for the cove we minted above. Pre-#290 the
    // stale cursor stranded the WS — no `cove.updated` would ever
    // land on the buffer.
    await waitForEvent(page, 'cove.updated', 15_000);

    // Sidebar entry should be visible — pin the cross-surface
    // contract: the WS resync feeds the React-Query cache the
    // sidebar reads from.
    await expect(
      page.locator('aside.side').getByRole('button', { name: /StaleCursorCove/i }),
    ).toBeVisible({ timeout: 5_000 });
  });
});
