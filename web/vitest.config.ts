// Vitest config — unit tests for the web app.
//
// Split of responsibilities:
//   - vitest (this file): fast, hermetic unit tests. No network, jsdom DOM,
//     mocked transports. Lives next to source under `src/**/*.test.{ts,tsx}`
//     and in the top-level `tests/` dir for setup-style files.
//   - playwright (`playwright.config.ts`): end-to-end tests against the
//     running docker stack at http://localhost:4040/calm/. Slower, requires
//     `make dev` first.
//
// Keep the surfaces non-overlapping: unit tests should never reach for the
// real server, and e2e tests should never reach into module internals.

import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./tests/setup.ts'],
    include: [
      'src/**/*.test.{ts,tsx}',
      'tests/**/*.test.{ts,tsx}',
      // The lint-rule smoke test sits next to the rule it tests
      // (`eslint-rules/no-persistent-in-usestate.test.ts`). Pulling it
      // under `src/` would muddle the source tree; instead, vitest
      // discovers it via this explicit glob.
      'eslint-rules/**/*.test.{ts,tsx}',
    ],
    // E2E specs live in ./e2e and are owned by playwright, not vitest.
    exclude: [
      '**/node_modules/**',
      '**/dist/**',
      'e2e/**',
    ],
    // Type-level tests live next to source as `*.test-d.ts`. They have no
    // runtime body; vitest invokes `tsc` over them via its typecheck mode.
    // The `Persistent<T>` brand guard (see `src/shared/state.test-d.ts`)
    // depends on this — without typecheck enabled, the brand could rot
    // silently. `tsc -b` (run during `npm run build`) catches the same
    // regressions, but we wire it here so `npm test` is a complete gate too.
    typecheck: {
      enabled: true,
      include: ['src/**/*.test-d.{ts,tsx}'],
      tsconfig: './tsconfig.app.json',
    },
  },
});
