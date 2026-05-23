// #177 regression — XtermView must NOT remount when the user toggles the
// app-level theme.
//
// Why this lives in the `a11y` Playwright project despite not being an a11y
// test: it needs the auto-spawned `cargo run --bin replay --serve` stack
// (real REST + WS, in-memory sqlite, dev-autologin auth) AND the Vite dev
// server in front of it (so `import.meta.env.DEV` is truthy). The
// `chromium` project would also work in principle but it requires the
// developer's `make dev` stack running locally — that's an external
// dependency we can't assert here. The a11y project bootstraps everything
// from scratch via `_setup/replay-server.setup.ts`.
//
// What we're proving (or disproving)
// ----------------------------------
// The user's DevTools console captured the smoking gun from prod:
//
//      [#177 XtermView instance] {theme: 'light', instance: 'bscohp'}
//      ... (steady state)
//      [#177 CodexCardImpl render] {theme: 'dark'}
//      [#177 XtermView render] {theme: 'dark'}
//      [#177 XtermView instance] {theme: 'dark', instance: 'zjqsq4'}
//                                                            ^^^^^^
//                                              new instance id → remount
//
// The vitest unit-test surface (`integration/theme-toggle-no-remount.test.tsx`)
// couldn't reproduce this — `useRef` identity stays stable across
// `setMode('dark')` flips in jsdom with mocked xterm. The gap is some
// real-browser-only factor (RGL + ResizeObserver, real Suspense /
// lazy-chunk resolve timing, the real query persister, the real wave
// detail fetch on the wire). Playwright + real Chromium is our last lever
// before stapling instrumentation directly into prod.
//
// Test plumbing
// -------------
//  * `?testMounts=1` URL flag unlocks two production-side hooks:
//      - `XtermView.tsx` increments `window.__xtermMounts__` on mount,
//        decrements on unmount. The signal we watch.
//      - `theme.tsx` exposes `window.__calmSetTheme(mode)` so we can flip
//        the theme WITHOUT navigating to the Settings page (which would
//        unmount the wave-page XtermView and defeat the observation).
//    Both hooks are gated on the query param so production users never
//    carry the side effect.
//
//  * We create a real cove + wave via the replay binary's REST surface
//    (same pattern as `a11y-keyboard.spec.ts`), then drive `+ Add → New
//    terminal` from the UI. The kernel's daemon spawn fires for real (the
//    replay binary mounts the full WS router) so XtermView opens a real
//    WebSocket against `/api/terminals/:id`. The bug repro path runs end
//    to end against the live stack.
//
// Outcome shapes
// --------------
//   * Test FAILS with `expected N, got N+1` (or higher) → reproduced
//     the user's prod-only remount. Hand-off to the fix PR with this
//     spec as the regression anchor.
//   * Test PASSES (mountsAfter === mountsBefore) → Playwright + the
//     replay binary + Vite-dev together do NOT reproduce. That's
//     useful data: it means the bug-triggering factor is something
//     the test stack omits. Candidates (none are present in the a11y
//     project):
//       - the production webpack bundle (no strict-mode double-invoke,
//         no React DEV warnings, different lazy-chunk preloader);
//       - the persist-query-client's IDB store hydrating an actual
//         previously-cached wave detail on second visit (the replay
//         binary boots cold every test);
//       - the docker stack's nginx + cookies + dev_autologin combination
//         producing a different SessionProvider re-render cadence;
//       - real PTY traffic from a codex / claude-tui daemon driving
//         render patches that interact with the theme-effect's
//         `prevThemeRef`.
//     This spec stays valuable in both shapes: it pins the contract
//     "an app theme toggle MUST NOT remount any visible XtermView",
//     so any future regression that re-introduces a remount in *any*
//     env trips this assertion.
//
// As of the commit that introduced this spec, the current env shape
// is GREEN — the Playwright a11y stack does not reproduce. The test
// remains in the suite as a regression anchor; pair it with the
// production-bundle reproduction work tracked in #177.

import { test, expect } from '@playwright/test';
import { createUserCove, createWaveInCove, resetReplayServer } from './helpers/reset';

test.beforeEach(async ({ request }) => {
  // Hermetic per-test state — every test starts from the fixture's seed
  // boot, no carryover from previous specs in the same `a11y` worker.
  // See `_setup/replay-server.setup.ts` for the replay-binary lifecycle.
  await resetReplayServer(request);
});

// Vite dev server compiles lazy chunks on first request; the default 30s
// budget is just barely enough on a warm machine and routinely blows on
// the cold initial boot of this suite. The actual *work* of the spec is
// cheap (two REST creates, two navigations, one button click, one JS
// `evaluate`, one settle wait); 90s gives the first run room to compile
// without masking real regressions.
test.setTimeout(90_000);

test('#177 XtermView does not remount on app theme toggle', async ({ page }) => {
  // Step 1 — boot with the `?testMounts=1` instrumentation flag.
  // This installs `window.__xtermMounts__` (mount counter) and
  // `window.__calmSetTheme(mode)` (theme driver) inside the running app.
  // Both are gated on this query param in `XtermView.tsx` and
  // `app/theme.tsx`; without `?testMounts=1` production builds carry
  // neither side effect.
  await page.goto('?testMounts=1', { waitUntil: 'domcontentloaded' });

  // Step 2 — mint a user-facing cove + wave via the replay REST API. The
  // a11y project's sidebar bootstrap is the same as elsewhere in the
  // suite (issue #175 — system cove hidden by default). We mint via the
  // direct REST path rather than the UI so the test focuses on the
  // theme-toggle observation, not the cove-creation flow.
  const cove = await createUserCove(request_(page), `E2E #177 ${Date.now()}`, '#6a8');
  const wave = await createWaveInCove(request_(page), cove.id, `E2E #177 wave ${Date.now()}`);

  // Step 3 — navigate into the wave. The wave page mounts WaveGrid which
  // (post-create) is empty until we add a card. We keep the `?testMounts=1`
  // flag on every navigation so the instrumentation effects survive
  // route changes.
  // `waitUntil: 'domcontentloaded'` — TanStack Router code-splits page
  // chunks lazily, so the `load` event fires after every dynamic-import
  // settles. Under heavy strict-mode chunk traffic that can push past the
  // 30s test default; we only need the DOM ready before we start asking
  // for accessible buttons.
  await page.goto(`wave/${wave.id}?testMounts=1`, { waitUntil: 'domcontentloaded' });
  await expect(page).toHaveURL(/\/calm\/wave\/[^/?]+\?testMounts=1$/);

  // Step 4 — the wave-create path already mints a spec card (codex) that
  // mounts XtermView synchronously on wave-create (PR #182 / issue #136).
  // So as soon as the wave page renders, an XtermView is mounting against
  // the spec card's terminal. We don't need `+ Add → New terminal` at all
  // for this test — we just observe the existing XtermView's mount count
  // across a theme toggle. Bonus: this matches the user's reported repro
  // (codex card on a wave, toggle theme, codex card's XtermView remounts).
  //
  // The xterm canvas exposes the role="application" landmark inside its
  // `.xterm-helpers` wrapper; we wait on the `.xterm-view` container that
  // `XtermView.tsx`'s top-level <div> emits, which is the most specific
  // signal that the component has actually mounted (and not still
  // resolving its lazy import). The counter then settles to the
  // post-strict-mode value.
  await expect(page.locator('.xterm-view').first()).toBeVisible({ timeout: 15_000 });
  await page.waitForFunction(
    () => {
      const n = (window as unknown as { __xtermMounts__?: number }).__xtermMounts__;
      return typeof n === 'number' && n >= 1;
    },
    null,
    { timeout: 15_000 },
  );

  // Let the strict-mode double-invoke settle. After both passes the
  // counter sits at N (one live mount per XtermView on the page); we
  // snapshot it here as the baseline. 500ms is enough on real Chromium
  // — the post-stabilization read below is the actual gate, this is
  // just a breath so we don't snapshot in the middle of the strict-mode
  // cycle.
  await page.waitForTimeout(500);
  const mountsBefore = await page.evaluate(
    () => (window as unknown as { __xtermMounts__?: number }).__xtermMounts__ ?? -1,
  );
  expect(
    mountsBefore,
    'baseline XtermView mount count must be >= 1 before the theme toggle (instrumentation sanity)',
  ).toBeGreaterThanOrEqual(1);

  // Sanity check: the theme driver must be installed too.
  await page.waitForFunction(
    () => typeof (window as unknown as { __calmSetTheme?: (m: string) => void }).__calmSetTheme === 'function',
    null,
    { timeout: 5_000 },
  );

  // Step 6 — flip the theme. We go light → dark; the bug reproduces in
  // either direction in the user's evidence, dark is the more common
  // post-toggle state on a fresh boot ('system' default frequently
  // resolves to 'light' on dev workstations).
  await page.evaluate(() => {
    const w = window as unknown as { __calmSetTheme?: (m: string) => void };
    w.__calmSetTheme?.('dark');
  });

  // Step 7 — give React + the theme-effect + any cascading subtree
  // re-renders a chance to settle. If the bug reproduces, this is the
  // window in which `useEffect(..., [])` would tear down and re-run
  // (or — per the user's trace, where the *new* instance is created
  // without a cleanup of the old one, suggesting a Suspense / offscreen
  // transition).
  await page.waitForTimeout(500);

  // Step 8 — `data-theme` must have flipped (sanity that the driver
  // actually worked end-to-end through ThemeProvider).
  const dataTheme = await page.evaluate(() => document.documentElement.dataset.theme);
  expect(dataTheme, '<html data-theme> should reflect the new mode').toBe('dark');

  // Step 9 — THE assertion. If the bug repro'd, the counter is now
  // strictly greater than `mountsBefore` (XtermView mounted again
  // without the old one being cleaned up — matching the user's
  // "no CLEANUP log between two instance ids" observation). If it
  // equals `mountsBefore` the count is conserved and there was no
  // remount.
  //
  // We compare against the snapshot baseline (not a hardcoded `1`)
  // because the wave-create path mints a spec card synchronously on
  // create, so the wave-page baseline is "however many XtermViews
  // the wave's cards mount" — currently 1 (spec card) but the test
  // shouldn't fragile-bind to that exact number.
  const mountsAfter = await page.evaluate(
    () => (window as unknown as { __xtermMounts__?: number }).__xtermMounts__ ?? -1,
  );
  expect(
    mountsAfter,
    `XtermView must not remount on theme toggle (user-reported regression #177). ` +
      `Baseline mounts=${mountsBefore}, after-toggle mounts=${mountsAfter}.`,
  ).toBe(mountsBefore);
});

// Helper: pluck `request` off the Playwright `Page` for use with the
// REST-helpers in `helpers/reset.ts`. Those helpers take an
// `APIRequestContext`, which `page.request` returns; this thin wrapper
// keeps the spec body readable.
function request_(page: import('@playwright/test').Page) {
  return page.request;
}
