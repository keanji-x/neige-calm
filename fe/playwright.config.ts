import { defineConfig, devices } from '@playwright/test';

const port = Number(process.env.FE_DEV_PORT ?? 5180);

export default defineConfig({
  testDir: './e2e',
  workers: 1,
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? [['github'], ['html', { open: 'never' }]] : 'list',
  use: {
    ...devices['Desktop Chrome'],
    baseURL: `http://127.0.0.1:${port}`,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  webServer: {
    command: `npm run dev -- --host 127.0.0.1 --port ${port} --strictPort`,
    url: `http://127.0.0.1:${port}/`,
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
    env: { FE_API_PROXY_TARGET: process.env.FE_API_PROXY_TARGET ?? 'http://127.0.0.1:4041' },
  },
});
