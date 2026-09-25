import { defineConfig } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import base from '../../playwright.config.ts';

const port = Number(process.env.FE_DEV_PORT ?? 5287);

// Exercise the shipped assets as well as the dev server covered by `npm run e2e`.
// Use one config object: defineConfig(base, overrides) concatenates webServers
// and would also start the dev server (colliding when FE_DEV_PORT is provided).
export default defineConfig({
  ...base,
  testDir: '../../e2e',
  testMatch: 'pwa.spec.ts',
  use: { ...base.use, baseURL: `http://127.0.0.1:${port}` },
  webServer: {
    command: `npm run build && npx vite preview --host 127.0.0.1 --port ${port} --strictPort`,
    cwd: fileURLToPath(new URL('../..', import.meta.url)),
    url: `http://127.0.0.1:${port}/next/`,
    reuseExistingServer: false,
    timeout: 120_000,
    // Pages are routed to a signed-out API response, but Chrome's background
    // fetch during PWA.install is not; point the preview proxy at a closed port
    // (discard, 9) so that fetch can never reach a real backend.
    env: { FE_API_PROXY_TARGET: 'http://127.0.0.1:9' },
  },
});
