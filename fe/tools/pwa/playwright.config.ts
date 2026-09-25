import { defineConfig } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import base from '../../playwright.config.ts';

const port = Number(process.env.FE_DEV_PORT ?? 5287);

// Exercise the shipped assets as well as the dev server covered by `npm run e2e`.
export default defineConfig(base, {
  testDir: '../../e2e',
  testMatch: 'pwa.spec.ts',
  use: { baseURL: `http://127.0.0.1:${port}` },
  webServer: {
    command: `npm run build && npx vite preview --host 127.0.0.1 --port ${port} --strictPort`,
    cwd: fileURLToPath(new URL('../..', import.meta.url)),
    url: `http://127.0.0.1:${port}/next/`,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
