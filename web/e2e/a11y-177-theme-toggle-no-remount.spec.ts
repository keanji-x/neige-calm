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
// Real-codex requirement (this revision)
// --------------------------------------
// An earlier shape of this spec used `CodexClient::new_stub()` as
// supplied by `replay::boot_in_memory` and simply asserted on mount
// counts. That env carried "the wave-create path schedules a spec
// card" but its daemon spawn failed with
//   `spawn calm-session-daemon: No such file or directory (os error 2)`
// — no PTY traffic, no RenderPatch events, no codex-driven re-render
// of the spec card. One of the candidate prod factors the docblock
// already flags is "real PTY traffic from a codex / claude-tui daemon
// driving render patches that interact with the theme-effect's
// `prevThemeRef`". Without it, this test reproduces a CodexClient-less
// stub world — not the user's prod reality. So this revision:
//
//   1. The `replay-setup` project pre-builds `calm-session-daemon` +
//      `neige-codex-bridge` into `target/debug/` (the daemon resolver
//      reads them as siblings of the running `replay` exe).
//   2. The `replay-setup` project probes for a usable `codex` CLI via
//      `resolveCodexBin` and prepends its directory to the PATH passed
//      to the cargo child. The resolution outcome is recorded to
//      `CODEX_BIN_FILE` so this spec can detect a codex-less machine.
//   3. THIS spec reads `CODEX_BIN_FILE` synchronously at module load
//      and `test.skip`s itself if codex isn't installed. Local-only
//      tests get a clear "codex CLI not installed" message; CI without
//      codex remains green.
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
//     replay binary + Vite-dev + real codex CLI together do NOT
//     reproduce. That's useful data: it means the bug-triggering
//     factor is something the test stack still omits. Remaining
//     candidates (none are present in the a11y project even with
//     real codex):
//       - the production webpack bundle (no strict-mode double-invoke,
//         no React DEV warnings, different lazy-chunk preloader);
//       - the persist-query-client's IDB store hydrating an actual
//         previously-cached wave detail on second visit (the replay
//         binary boots cold every test);
//       - the docker stack's nginx + cookies + dev_autologin combination
//         producing a different SessionProvider re-render cadence;
//       - the production xterm dimensions / font-loading timing that
//         differs between Vite-dev's "no font preload" and Nginx's
//         hashed-asset cache headers.
//     This spec stays valuable in both shapes: it pins the contract
//     "an app theme toggle MUST NOT remount any visible XtermView",
//     so any future regression that re-introduces a remount in *any*
//     env trips this assertion.
//
//   * Test SKIPPED ("codex CLI not installed") → setup-time `which
//     codex` came up empty AND the pinned `~/.nvm/...` candidate
//     wasn't there either. Install codex (`npm i -g @openai/codex`)
//     or point `CALM_CODEX_BIN` at the binary to opt in. CI that
//     doesn't install codex stays green here — the spec doesn't
//     guard against a regression we can't observe without the
//     dependency.

import { readFileSync } from 'node:fs';
import { test, expect } from '@playwright/test';
import { createUserCove, createWaveInCove, resetReplayServer } from './helpers/reset';
import { CODEX_BIN_FILE, CODEX_MISSING_SENTINEL } from './_setup/replay-server.shared';

// Synchronous module-load probe so `test.skip(...)` can run at the
// right time (before the `beforeEach` / test body). The marker file is
// written by `replay-server.setup.ts` as part of the `replay-setup`
// dependency project; by the time this spec module evaluates it MUST
// exist (Playwright wouldn't have reached the a11y project otherwise).
// We tolerate a missing file as "skip" rather than throw — running
// this spec file standalone (without the setup project) shouldn't
// crash; it should just self-skip with a useful message.
const codexResolution = readCodexResolution();
const codexAvailable =
  codexResolution !== null && codexResolution !== CODEX_MISSING_SENTINEL;

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
// `evaluate`, one settle wait); 120s gives the first run room to compile
// AND for codex's first-paint to land — codex boots a real Node/Rust
// process and its first OSC + render-patch cycle is ~1-2s on a warm box.
test.setTimeout(120_000);

test('#177 XtermView does not remount on app theme toggle', async ({ page }) => {
  // Hard gate — without a usable codex binary the wave-create spec
  // card spawns a daemon that can't exec its argv, no PTY traffic,
  // no production-shape repro. Skip with a clear message; do NOT
  // proceed to assert on a degraded env (false negatives would hide
  // future regressions when codex is reinstated).
  test.skip(
    !codexAvailable,
    `codex CLI not installed (looked at CALM_CODEX_BIN, ~/.nvm/versions/node/*/bin/codex, ~/.local/bin/codex, login-shell PATH). ` +
      `Install codex or set CALM_CODEX_BIN to opt in. Resolution marker: ${codexResolution ?? '<missing>'}.`,
  );

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

  // Step 4b — wait for ACTUAL PTY traffic to land. The whole point of
  // the real-codex requirement is that codex's render patches reach
  // XtermView and drive the same xterm.js render path that exists in
  // prod. We poll the `.xterm-rows` container for non-empty text
  // content; xterm.js renders one `<div>` per visible terminal row
  // and only writes the row spans once a screen update arrives. Empty
  // rows = WS connected but no daemon output yet (= we'd toggle theme
  // against a static null screen, which is the very degenerate state
  // the unit tests already covered without seeing the bug).
  //
  // 30s is generous: codex on a warm box first-paints within ~1-2s
  // (the startup banner + `> ` prompt are usually the first OSC +
  // RenderPatch pair). 30s covers cold-boot CI runners where codex's
  // first parse of `~/.codex/` config dominates.
  await page.waitForFunction(
    () => {
      const rows = document.querySelector('.xterm-rows');
      if (!rows) return false;
      const text = (rows as HTMLElement).innerText ?? '';
      return text.trim().length > 0;
    },
    null,
    { timeout: 30_000 },
  );

  // Let the strict-mode double-invoke settle + give codex's
  // post-first-paint cycle a moment to quiesce. After both passes the
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

// Helper: synchronous read of the codex-resolution marker file written
// by `_setup/replay-server.setup.ts`. Returns the marker string on
// success, or `null` if the file isn't there (running this spec file
// standalone, ahead of the setup project, or after a manual cache
// wipe). The caller treats `null` and the missing-sentinel identically
// — `test.skip` either way — so the only thing we lose by tolerating a
// missing file is an ability to distinguish "setup hasn't run" from
// "setup ran and codex was absent". For the spec's purpose that
// distinction doesn't matter; the message we emit covers both.
function readCodexResolution(): string | null {
  try {
    const raw = readFileSync(CODEX_BIN_FILE, 'utf8').trim();
    return raw.length === 0 ? null : raw;
  } catch {
    return null;
  }
}
