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
          /*
           * `*.coarse.browser.test.*` is a `*.browser.test.*`, so this glob
           * collects it too and the tier gate's "exactly one project" rule
           * would fail on it. The exclude is what makes the partition a
           * partition; it is not an optimisation.
           */
          exclude: ['**/*.coarse.browser.test.{ts,tsx}', 'tools/architecture/fixtures/**'],
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
      /*
       * ── The coarse pointer, in a browser context of its own ──────────────
       *
       * `pointer: coarse` is a media *feature*, and the only thing that can set
       * one is the driver. The obvious lever — `Emulation.setTouchEmulationEnabled`
       * over CDP — is a **one-way door**: turning it on gives `pointer: coarse`,
       * and turning it off leaves the page at `pointer: none`, where neither
       * branch of the rail's geometry matches. Vitest's browser mode reuses one
       * page for every file in a project, so a single case that opened that door
       * poisoned every case after it, in every file. That is why the coarse
       * branch of `thread.module.css` was for three rounds guarded by reading its
       * own `cssText` out of `document.styleSheets` and never rendered.
       *
       * A separate project is the way out, because Playwright's context options
       * are per *context*: `hasTouch` + `isMobile` produce a page that reports
       * `pointer: coarse` from its first paint, and no page shared with the
       * `browser` project is touched. Measured under this context: `pointer:
       * coarse` matches, `pointer: none` does not, and the plain `browser`
       * project still reports `pointer: fine` with `pointer: none` false.
       *
       * The viewport is a phone's, and it is not decoration: `isMobile` without
       * a matching viewport is a desktop-sized page claiming to be a handset,
       * and the rail's 320px cap is a block-axis fact that only means anything
       * against a real screen height.
       */
      {
        test: {
          name: 'browser-coarse',
          include: ['**/*.coarse.browser.test.{ts,tsx}'],
          setupFiles,
          browser: {
            enabled: true,
            headless: true,
            provider: defineBrowserProvider(playwright({
              contextOptions: {
                hasTouch: true,
                isMobile: true,
                viewport: { width: 420, height: 900 },
              },
            })),
            instances: [{ browser: 'chromium' }],
          },
        },
      },
    ],
  },
});
