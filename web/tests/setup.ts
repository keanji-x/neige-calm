// Vitest setup — runs once before each test file (per `setupFiles` in
// vitest.config.ts). Adds the `@testing-library/jest-dom` matchers
// (e.g. `toBeInTheDocument`, `toHaveTextContent`) to vitest's `expect`.
//
// Keep this file tiny — heavy per-test setup belongs in the test, not here.
import '@testing-library/jest-dom/vitest';
