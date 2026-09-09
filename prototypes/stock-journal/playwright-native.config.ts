import { defineConfig } from '@playwright/test';

// Exercise the actual fe production entry point with API-boundary fixtures.
// The prototype app, mode switch and iframe bundle are not part of this run.
export default defineConfig({
  testDir: './tests', testMatch: 'live-market.spec.ts', workers: 1,
  use: { baseURL: 'http://127.0.0.1:5197', browserName: 'chromium', viewport: { width: 1440, height: 1000 } },
  webServer: {
    command: 'cd ../../fe && node node_modules/vite/bin/vite.js preview --host 127.0.0.1 --port 5197 --strictPort',
    url: 'http://127.0.0.1:5197/next/', reuseExistingServer: false,
  },
});
