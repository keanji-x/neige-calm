import { defineConfig } from 'vitest/config';
import { defineBrowserProvider } from '@vitest/browser';
import { playwright } from '@vitest/browser-playwright';

import { OPTIMIZED_DEPENDENCIES } from './tools/vitest/optimized-dependencies.ts';

// Each of these no-ops where it does not apply, so the one list feeds every project.
const setupFiles = [
  './tools/vitest/build-constants.ts',
  './tools/vitest/dom-diagnostics.ts',
  './tools/vitest/jsdom-match-media.ts',
  './tools/vitest/jsdom-resize-observer.ts',
];

export default defineConfig({
  resolve: {
    dedupe: ['react', 'react-dom'],
  },
  optimizeDeps: {
    include: [...OPTIMIZED_DEPENDENCIES],
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
        optimizeDeps: {
          // Browser projects are isolated Vite configs and do not inherit the
          // root list, so use the complete roster rather than a partial copy.
          include: [...OPTIMIZED_DEPENDENCIES],
        },
        test: {
          name: 'browser',
          include: ['**/*.browser.test.{ts,tsx}'],
          /* `*.coarse.browser.test.*` is a `*.browser.test.*` too; the exclude is what makes the partition a partition. */
          exclude: ['**/*.coarse.browser.test.{ts,tsx}', 'tools/architecture/fixtures/**'],
          setupFiles,
          browser: {
            enabled: true,
            headless: true,
            provider: defineBrowserProvider(playwright()),
            instances: [{ browser: 'chromium' }],
            /* `prefers-reduced-motion` is a media feature only the driver can emulate, so it is exposed as a command. */
            commands: {
              emulateReducedMotion: async ({ page }, reduce: boolean) => {
                await page.emulateMedia({ reducedMotion: reduce ? 'reduce' : 'no-preference' });
              },
            },
          },
        },
      },
      /* `pointer: coarse` needs its own project: `Emulation.setTouchEmulationEnabled` is a one-way door and Vitest reuses one page per project.
         `contextOptions.viewport` sizes the page; `browser.viewport` sizes the iframe, which is what `@media (width < 60rem)` is evaluated against. */
      {
        optimizeDeps: {
          include: [...OPTIMIZED_DEPENDENCIES],
        },
        test: {
          name: 'browser-coarse',
          include: ['**/*.coarse.browser.test.{ts,tsx}'],
          /* `tools/architecture/fixtures/**` holds deliberately broken sources, and `check-test-tier.mjs` drops
           * it from `trackedTests`, so a coarse-suffixed file there would run while invisible to the partition check. */
          exclude: ['tools/architecture/fixtures/**'],
          setupFiles,
          browser: {
            enabled: true,
            headless: true,
            /* The suite's iframe; at the 414 × 896 default every case would lay out under `@media (width < 60rem)`. */
            viewport: { width: 1024, height: 1366 },
            provider: defineBrowserProvider(playwright({
              contextOptions: {
                hasTouch: true,
                isMobile: true,
                viewport: { width: 1024, height: 1366 },
              },
            })),
            instances: [{ browser: 'chromium' }],
          },
        },
      },
    ],
  },
});
