import { defineConfig } from 'vitest/config';
import { defineBrowserProvider } from '@vitest/browser';
import { playwright } from '@vitest/browser-playwright';

// See `tools/vitest/build-constants.ts` for why this is a setup file and not a
// second `define` block.
const setupFiles = ['./tools/vitest/build-constants.ts'];

export default defineConfig({
  resolve: {
    dedupe: ['react', 'react-dom'],
  },
  optimizeDeps: {
    include: [
      '@astryxdesign/core/Calendar',
      '@astryxdesign/core/Icon',
      '@astryxdesign/core/IconButton',
      '@astryxdesign/core/List',
      '@astryxdesign/core/MoreMenu',
    ],
  },
  test: {
    projects: [
      {
        test: {
          name: 'platform-independent',
          environment: 'node',
          setupFiles,
          include: ['core/**/*.test.ts', 'tools/**/*.test.ts'],
          exclude: ['**/*.browser.test.{ts,tsx}', 'tools/architecture/fixtures/**'],
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
          exclude: ['tools/architecture/fixtures/**'],
          setupFiles,
          browser: {
            enabled: true,
            headless: true,
            provider: defineBrowserProvider(playwright()),
            instances: [{ browser: 'chromium' }],
            /*
             * `prefers-reduced-motion` is a *media feature*, and nothing inside
             * the page can set one — which is how a reduced-motion rule shipped
             * that never matched anything: the test that guarded it read the
             * rule's own text out of the stylesheet instead of asking the
             * engine what it computed. Only the driver can emulate the feature,
             * so it is exposed here as a command and the assertion moves to the
             * effective `animation-name` on the painted element.
             */
            commands: {
              emulateReducedMotion: async ({ page }, reduce: boolean) => {
                await page.emulateMedia({ reducedMotion: reduce ? 'reduce' : 'no-preference' });
              },
            },
          },
        },
      },
    ],
  },
});
