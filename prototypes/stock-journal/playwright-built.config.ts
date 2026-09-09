import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  testMatch: ['built-preview.spec.ts', 'live-market.spec.ts'],
  workers: 1,
  use: { baseURL: 'http://127.0.0.1:5195', browserName: 'chromium', viewport: { width: 1280, height: 1000 } },
  webServer: {
    command: 'node node_modules/vite/bin/vite.js preview --host 127.0.0.1 --port 5195 --strictPort',
    url: 'http://127.0.0.1:5195/next/track/portfolio',
    reuseExistingServer: !process.env.CI,
  },
});
