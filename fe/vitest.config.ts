import { defineConfig } from 'vitest/config';
import { defineBrowserProvider } from '@vitest/browser';
import { playwright } from '@vitest/browser-playwright';

// See `tools/vitest/build-constants.ts` for why this is a setup file and not a
// second `define` block.
const setupFiles = ['./tools/vitest/build-constants.ts'];

export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: 'platform-independent',
          environment: 'node',
          setupFiles,
          include: ['core/**/*.test.ts', 'tools/**/*.test.ts'],
          exclude: ['**/*.browser.test.{ts,tsx}'],
        },
      },
      {
        test: {
          name: 'web-dom',
          environment: 'jsdom',
          setupFiles,
          include: ['web/src/**/*.test.{ts,tsx}'],
          exclude: ['**/*.browser.test.{ts,tsx}'],
        },
      },
      {
        test: {
          name: 'browser',
          include: ['**/*.browser.test.{ts,tsx}'],
          setupFiles,
          browser: {
            enabled: true,
            headless: true,
            provider: defineBrowserProvider(playwright()),
            instances: [{ browser: 'chromium' }],
          },
        },
      },
    ],
  },
});
