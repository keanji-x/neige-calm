// Unit tests for the `isCompatible` helper in `api/version.ts`.
//
// The function is one line, but the comparison direction is the kind of
// thing that's easy to flip during a refactor and not notice until a
// real deploy serves an incompatible tab. These cases lock the contract
// to the table in `docs/upgrade-stability.md`:
//
//   - server v1, frontend v1 → compatible
//   - server v2, frontend v1 → incompatible (frontend below floor)
//   - server v1, frontend v2 → compatible (frontend ahead is fine)
//
// `fetchServerVersion` isn't exercised here — it's a one-line wrapper
// over `fetch`, and its real contract is the integration with the
// backend, which is covered by the backend's own /api/version tests.

import { describe, it, expect } from 'vitest';
import { isCompatible } from './version';

describe('isCompatible', () => {
  it('returns true when server min equals frontend version', () => {
    expect(isCompatible({ minWebCompatVersion: 1 }, 1)).toBe(true);
  });

  it('returns false when frontend is below the server minimum', () => {
    // Server has rolled to v2; this tab still ships v1. The overlay
    // should hard-block this case in the app.
    expect(isCompatible({ minWebCompatVersion: 2 }, 1)).toBe(false);
  });

  it('returns true when frontend is ahead of the server minimum', () => {
    // Server still accepts the older contract (its minimum is v1);
    // a v2 frontend is allowed through.
    expect(isCompatible({ minWebCompatVersion: 1 }, 2)).toBe(true);
  });
});
