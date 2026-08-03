import { defineConfig } from 'vitest/config';
import { defineBrowserProvider } from '@vitest/browser';
import { playwright } from '@vitest/browser-playwright';

export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: 'platform-independent',
          environment: 'node',
          include: ['core/**/*.test.ts', 'tools/**/*.test.ts', 'web/src/**/*.test.{ts,tsx}'],
          exclude: ['**/*.browser.test.{ts,tsx}', 'web/src/ui/**/*.test.{ts,tsx}'],
        },
      },
      {
        test: {
          name: 'ui-dom',
          environment: 'jsdom',
          include: ['web/src/ui/**/*.test.{ts,tsx}'],
          exclude: ['**/*.browser.test.{ts,tsx}'],
        },
      },
      {
        test: {
          name: 'browser',
          include: ['**/*.browser.test.{ts,tsx}'],
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
