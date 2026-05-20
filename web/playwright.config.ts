// Playwright config — end-to-end browser tests for the calm web UI.
//
// Counterpart to `vitest.config.ts`:
//   - vitest = fast, hermetic, no network, jsdom. Unit tests for adapters,
//     schemas, and hooks. See `src/**/*.test.{ts,tsx}` + `tests/`.
//   - playwright (this file) = real browser against the running docker
//     stack at http://localhost:4040/calm/. Slow but covers the WS + REST
//     + router seams end-to-end.
//
// Prereqs: bring the stack up first with `make dev` in the repo root, then
// run `npm run e2e` here. We deliberately don't set `webServer` — booting
// the full kernel + sqlite seed from playwright would be slower than the
// human dev loop and would race with `make dev` if already running.

import { defineConfig, devices } from '@playwright/test';

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
    baseURL: 'http://localhost:4040/calm/',
    // Capture artifacts only on failure to keep the local run cheap.
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
});
