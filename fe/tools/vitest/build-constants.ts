/*
 * §0.5's build-time constants, supplied to the test run.
 *
 * `vite.config.ts` injects `__NC_VERSION__` / `__NC_BUILD__` through `define`,
 * but Vitest is configured by `vitest.config.ts` and never loads that file, so
 * under test the two globals simply did not exist. Settings' ABOUT section
 * reads both, so every test that rendered Settings died on a `ReferenceError`
 * before it could assert anything — 17 failures across two files, one cause.
 *
 * A setup file rather than a second `define`: Vitest's SSR transform does not
 * apply `define` to these identifiers (tried, at the root and per project), and
 * a global assignment is in any case the more honest model of what they are —
 * ambient facts about the artifact, absent unless something supplies them.
 *
 * The values are deliberately not the real version and hash. A test that
 * asserted the true build would fail on every commit; what the ABOUT section
 * owes its tests is that it renders whatever it is given.
 */
declare global {
  var __NC_VERSION__: string;
  var __NC_BUILD__: string;
}

globalThis.__NC_VERSION__ = '0.0.0-test';
globalThis.__NC_BUILD__ = 'test';

export {};
