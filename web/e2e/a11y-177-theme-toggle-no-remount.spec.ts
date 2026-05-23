// #177 regression — XtermView must NOT remount when the user toggles
// the app-level theme.
//
// Why this lives in the `a11y` Playwright project despite not being an
// a11y test: it needs the auto-spawned `cargo run --bin replay --serve`
// stack (real REST + WS, in-memory sqlite, dev-autologin auth) AND the
// Vite dev server in front of it. The `chromium` project would also
// work in principle but requires the developer's `make dev` stack
// running locally; the `a11y` project bootstraps everything from
// scratch via `_setup/replay-server.setup.ts`.
//
// What this spec pins
// -------------------
// The user's DevTools console captured an XtermView remount on every
// theme toggle:
//
//   [#177 XtermView instance] {theme: 'light', instance: 'bscohp'}
//   ... (steady state)
//   [#177 CodexCardImpl render] {theme: 'dark'}
//   [#177 XtermView render] {theme: 'dark'}
//   [#177 XtermView instance] {theme: 'dark', instance: 'zjqsq4'}
//                                                       ^^^^^^
//                                          new instance id → remount
//
// Vitest unit + integration tests (`XtermView.test.tsx`,
// `integration/theme-toggle-no-remount.test.tsx`) couldn't reproduce
// this in jsdom — the gap is some real-browser-only factor (RGL +
// ResizeObserver, real Suspense / lazy-chunk resolve timing, the real
// query persister, the real wave-detail fetch on the wire). Playwright
// + real Chromium is our last lever before stapling instrumentation
// directly into prod.
//
// Test plumbing
// -------------
//  * `?testMounts=1` URL flag unlocks two production-side hooks
//    (zero-cost without the flag):
//      - `XtermView.tsx` increments `window.__xtermMounts__` on mount,
//        decrements on unmount. The signal we watch.
//      - `theme.tsx` exposes `window.__calmSetTheme(mode)` so we can
//        flip the theme WITHOUT navigating to the Settings page
//        (navigation would unmount any wave-page XtermView and defeat
//        the whole observation).
//
//  * We create a real cove + wave via the replay binary's REST surface
//    (same pattern as `a11y-keyboard.spec.ts`), then rely on the
//    sync-spawn spec-card path (#136 PR6) to mint an XtermView when
//    the wave page renders. No `+ Add → New terminal` step needed.
//
// Outcome shapes
// --------------
//   * Test FAILS with `expected N, got N+1` (or higher) → reproduced
//     the user-reported regression. Hand-off to the fix PR with this
//     spec as the regression anchor.
//   * Test PASSES → Playwright + the replay binary + Vite-dev together
//     do not reproduce the production remount. The spec still pins
//     the contract "an app theme toggle MUST NOT remount any visible
//     XtermView", so any future regression that re-introduces a
//     remount in *any* env trips this assertion.
//   * Test SKIPS — see the codex-availability gate below.

import { readFileSync } from 'node:fs';
import { test, expect, type APIRequestContext, type Page } from '@playwright/test';
import {
  createUserCove,
  createWaveInCove,
  resetReplayServer,
} from './helpers/reset';
import { CODEX_BIN_FILE, CODEX_MISSING_SENTINEL } from './_setup/replay-server.shared';

// Synchronous module-load probe so `test.skip` can run at the right
// time (before the test body). The marker file is written by
// `replay-server.setup.ts`; by the time this spec module evaluates it
// MUST exist (Playwright wouldn't have reached the a11y project
// otherwise). A missing file is treated as "skip" rather than throw —
// running this spec standalone (without the setup project) self-skips
// with a useful message.
const codexResolution = readCodexResolution();
const codexAvailable =
  codexResolution !== null && codexResolution !== CODEX_MISSING_SENTINEL;

test.beforeEach(async ({ request }) => {
  await resetReplayServer(request);
});

// Vite dev server compiles lazy chunks on first request; the default
// 30s budget is just barely enough on a warm machine and routinely
// blows on the cold initial boot of this suite. 60s gives the first
// run room to compile.
test.setTimeout(60_000);

test('#177 XtermView does not remount on app theme toggle', async ({
  page,
}) => {
  // The spec is most diagnostic with real PTY output flowing into
  // XtermView (so the theme toggle interacts with a non-degenerate
  // render path), and that means we need a real codex on disk for the
  // spec card's daemon. Skip cleanly when it isn't available — CI
  // without codex stays green; the unit + integration tests still
  // pin the contract in isolation.
  test.skip(
    !codexAvailable,
    `codex CLI not installed (looked at CALM_CODEX_BIN, ~/.nvm/versions/node/*/bin/codex, ~/.local/bin/codex, login-shell PATH). ` +
      `Install codex (or set CALM_CODEX_BIN) to opt in. Resolution marker: ${codexResolution ?? '<missing>'}.`,
  );

  // Step 1 — boot with the `?testMounts=1` instrumentation flag.
  await page.goto('?testMounts=1', { waitUntil: 'domcontentloaded' });

  // Step 2 — mint a user-facing cove + wave via the replay REST API.
  const cove = await createUserCove(request_(page), `E2E #177 ${Date.now()}`, '#6a8');
  const wave = await createWaveInCove(
    request_(page),
    cove.id,
    `E2E #177 wave ${Date.now()}`,
  );

  // Step 3 — navigate into the wave. Keep `?testMounts=1` on every
  // navigation so the instrumentation survives route changes.
  await page.goto(`wave/${wave.id}?testMounts=1`, {
    waitUntil: 'domcontentloaded',
  });
  await expect(page).toHaveURL(/\/calm\/wave\/[^/?]+\?testMounts=1$/);

  // Step 4 — the wave-create path mints a spec card synchronously
  // (#136 PR6 / #182), which mounts XtermView. We wait for the
  // `.xterm-view` element + the `__xtermMounts__` counter to settle.
  await expect(page.locator('.xterm-view').first()).toBeVisible({
    timeout: 15_000,
  });
  await page.waitForFunction(
    () => {
      const n = (window as unknown as { __xtermMounts__?: number })
        .__xtermMounts__;
      return typeof n === 'number' && n >= 1;
    },
    null,
    { timeout: 15_000 },
  );

  // Settle past the strict-mode double-invoke before snapshotting the
  // baseline. 500ms is generous for real Chromium.
  await page.waitForTimeout(500);
  const mountsBefore = await page.evaluate(
    () =>
      (window as unknown as { __xtermMounts__?: number }).__xtermMounts__ ??
      -1,
  );
  expect(
    mountsBefore,
    'baseline XtermView mount count must be >= 1 before the toggle',
  ).toBeGreaterThanOrEqual(1);

  // Sanity: the theme driver must be installed.
  await page.waitForFunction(
    () =>
      typeof (
        window as unknown as { __calmSetTheme?: (m: string) => void }
      ).__calmSetTheme === 'function',
    null,
    { timeout: 5_000 },
  );

  // Step 5 — flip the theme via the instrumented driver.
  await page.evaluate(() => {
    const w = window as unknown as { __calmSetTheme?: (m: string) => void };
    w.__calmSetTheme?.('dark');
  });
  await page.waitForTimeout(500);

  // Step 6 — sanity-check that the toggle landed end-to-end.
  const dataTheme = await page.evaluate(
    () => document.documentElement.dataset.theme,
  );
  expect(dataTheme, '<html data-theme> should reflect the new mode').toBe(
    'dark',
  );

  // Step 7 — THE assertion: mount counter must be conserved.
  const mountsAfter = await page.evaluate(
    () =>
      (window as unknown as { __xtermMounts__?: number }).__xtermMounts__ ??
      -1,
  );
  expect(
    mountsAfter,
    `XtermView must not remount on theme toggle (#177). ` +
      `Baseline mounts=${mountsBefore}, after-toggle mounts=${mountsAfter}.`,
  ).toBe(mountsBefore);
});

function request_(page: Page): APIRequestContext {
  return page.request;
}

/** Synchronous read of the codex-resolution marker written by
 *  `_setup/replay-server.setup.ts`. Returns the marker string on
 *  success, or `null` if the file isn't there. */
function readCodexResolution(): string | null {
  try {
    const raw = readFileSync(CODEX_BIN_FILE, 'utf8').trim();
    return raw.length === 0 ? null : raw;
  } catch {
    return null;
  }
}
