import { defineConfig } from 'vitest/config';
import { defineBrowserProvider } from '@vitest/browser';
import { playwright } from '@vitest/browser-playwright';

// See `tools/vitest/build-constants.ts` for why this is a setup file and not a
// second `define` block.
// `dom-diagnostics.ts` no-ops outside the DOM projects, so it can live in the
// shared list — a project added later gets the #1161 failure report for free.
const setupFiles = ['./tools/vitest/build-constants.ts', './tools/vitest/dom-diagnostics.ts'];

export default defineConfig({
  resolve: {
    dedupe: ['react', 'react-dom'],
  },
  optimizeDeps: {
    include: [
      '@tanstack/react-query',
      '@tanstack/react-router',
      '@astryxdesign/core/Button',
      '@astryxdesign/core/Calendar',
      '@astryxdesign/core/Card',
      '@astryxdesign/core/Heading',
      '@astryxdesign/core/Icon',
      '@astryxdesign/core/IconButton',
      '@astryxdesign/core/List',
      '@astryxdesign/core/MetadataList',
      '@astryxdesign/core/MoreMenu',
      '@astryxdesign/core/SegmentedControl',
      '@astryxdesign/core/TextInput',
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
       * ── The device is a tablet, and that is a ruling, not a preference ────
       *
       * This project ran at a phone's 420 × 900 until #1191, and it had to
       * stop. `pointer: coarse` means *a finger*; it does not mean *a narrow
       * screen*, and the two were conflated here only because a handset is the
       * cheapest device that has one. #1191's mobile IA then made the
       * difference load-bearing: `ui/drawer/drawer.module.css` turns the whole
       * page seam off under `@media (width < 60rem)`, because a full-width
       * mobile Chat has no seam beside the card for the desktop exchange rail
       * to mount in. Both features are right, and at a phone width they are not
       * simultaneously observable — the rail's geometry was being measured on a
       * page that, since #1191, deliberately does not paint it, so all three
       * geometry cases read 0.
       *
       * A tablet in portrait is where the rail's real invariants live: a finger
       * on the glass *and* a seam to reach into. 1024 × 1366 is chosen, and
       * every part of it is doing work —
       *
       *   - **1024 ≥ 60rem**, so the drawer keeps its desktop side-card shape
       *     and the seam exists. This is the number #1191 turns on; do not put
       *     this project back under it.
       *   - **1366 tall**, because the rail's 320px cap is a block-axis fact
       *     that only means anything against a real screen height — the reason
       *     the old comment gave for pinning a viewport at all, which still
       *     holds.
       *   - **portrait**, not landscape, because one case in the file below
       *     injects `@media (pointer: coarse) and (orientation: landscape)` as
       *     a rule that is false *here* and true on the same device turned. In
       *     landscape that fixture would match and would stop binding the half
       *     of the sweep it exists for.
       *
       * `isMobile` stays: a tablet is a mobile-emulation device too (it is what
       * Playwright's own iPad descriptors set), and it is what makes
       * `screen.orientation` report the emulated box rather than the host's.
       *
       * **Two viewports, and they are different things.** `contextOptions.
       * viewport` sizes the Playwright *page*, which is what `screen` and the
       * media features are read off. `browser.viewport` sizes the *iframe*
       * Vitest puts each suite in, which defaults to 414 × 896 whatever the
       * context says — and it is the iframe, not the screen, that `@media
       * (width < 60rem)` is evaluated against. Measured: widening the context
       * alone leaves all three geometry cases at 0.
       */
      {
        test: {
          name: 'browser-coarse',
          include: ['**/*.coarse.browser.test.{ts,tsx}'],
          /*
           * The same exclude the other two collecting projects carry, for the
           * same reason and one more. `tools/architecture/fixtures/**` holds
           * deliberately-broken sources the lint rules are pointed at, so
           * running them is meaningless; and `check-test-tier.mjs` drops that
           * directory from `trackedTests`, so a coarse-suffixed file under it
           * would be executed for real while being invisible to the partition
           * check that would otherwise have caught it.
           */
          exclude: ['tools/architecture/fixtures/**'],
          setupFiles,
          browser: {
            enabled: true,
            headless: true,
            /* The suite's iframe. Left at the 414 × 896 default, every case
               below is laid out under `@media (width < 60rem)` however wide the
               page behind it is. */
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
