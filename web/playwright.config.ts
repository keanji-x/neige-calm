// Playwright config — end-to-end browser tests for the calm web UI.
//
// Counterpart to `vitest.config.ts`:
//   - vitest = fast, hermetic, no network, jsdom. Unit tests for adapters,
//     schemas, and hooks. See `src/**/*.test.{ts,tsx}` + `tests/`.
//   - playwright (this file) = real browser against a running calm-server.
//     Slow but covers the WS + REST + router seams end-to-end.
//
// Two Playwright projects share this config:
//
//   * `chromium` — pre-existing project. Targets the developer's
//     `make dev` stack at http://localhost:4040/calm/. We deliberately
//     don't set `webServer` here either; booting the full kernel +
//     sqlite seed from playwright would be slower than the human dev
//     loop and would race with `make dev` if already running. Specs:
//     `golden-path.spec.ts`, `wave-create.spec.ts`.
//
//   * `a11y` — issue #56 slice 5. Targets the in-process replay binary
//     (`cargo run --bin replay -- --serve`) spawned by the `replay-setup`
//     dependency project in `_setup/replay-server.ts`, preloaded with a
//     curated event-trace fixture. Use this project for tests that need
//     the event trace ring buffer (`window.__neigeEvents__`) — they
//     assert role/name state AND the WS event sequence that produced it.
//     Specs: `a11y-trace-smoke.spec.ts` (and Slice 6's a11y/axe specs
//     once they land).
//
// The replay binary is spawned exclusively by the `replay-setup` project,
// which only runs as a dependency of `a11y`. That means `npx playwright
// test --project=chromium` (the existing local dev loop) still doesn't
// require cargo on PATH; only `--project=a11y` (or the default "run all
// projects" flow) does.

import { defineConfig, devices } from '@playwright/test';

const REPLAY_PORT = 4141;
const REPLAY_BASE_URL = `http://127.0.0.1:${REPLAY_PORT}/calm/`;

export default defineConfig({
  testDir: './e2e',
  // Two retries in CI helps with flaky animation timings; locally we fail
  // fast so the dev loop stays tight.
  retries: process.env.CI ? 2 : 0,
  // One worker keeps cove/wave seed state predictable — multiple workers
  // would mutate the same MockRepo concurrently.
  workers: 1,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    // Capture artifacts only on failure to keep the local run cheap.
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  projects: [
    {
      // Setup-only project. Its single "test" (defined in
      // `replay-server.setup.ts`) boots the cargo replay binary. The
      // `teardown` reference here makes Playwright run the matching
      // teardown project even if a downstream test fails or the run is
      // interrupted (Ctrl-C).
      name: 'replay-setup',
      testMatch: /e2e\/_setup\/replay-server\.setup\.ts$/,
      teardown: 'replay-teardown',
    },
    {
      name: 'replay-teardown',
      testMatch: /e2e\/_setup\/replay-server\.teardown\.ts$/,
    },
    {
      name: 'chromium',
      testIgnore: ['**/a11y-*.spec.ts', '**/_setup/**'],
      use: {
        ...devices['Desktop Chrome'],
        baseURL: 'http://localhost:4040/calm/',
      },
    },
    {
      name: 'a11y',
      testMatch: ['**/a11y-*.spec.ts'],
      dependencies: ['replay-setup'],
      use: {
        ...devices['Desktop Chrome'],
        baseURL: REPLAY_BASE_URL,
      },
    },
  ],
});
