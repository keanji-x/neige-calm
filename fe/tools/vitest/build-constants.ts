/* Build-time constants for the test run. `vite.config.ts` injects these through `define`, but Vitest
 * never loads that file and its SSR transform does not apply `define` to them, hence a setup file. */
declare global {
  var __NC_VERSION__: string;
  var __NC_BUILD__: string;
  var __NC_BUNDLED__: boolean;
}

globalThis.__NC_VERSION__ = '0.0.0-test';
globalThis.__NC_BUILD__ = 'test';
globalThis.__NC_BUNDLED__ = false;

export {};
