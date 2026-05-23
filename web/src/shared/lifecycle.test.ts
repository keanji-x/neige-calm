import { describe, expect, it } from 'vitest';

import type { WaveLifecycle } from '../types';
import { isRunning, isWaitingForUser } from './lifecycle';

// Exhaustive mapping pinned to a `Record<WaveLifecycle, ...>` literal:
// adding a new variant to `WaveLifecycle` without a row here is a type
// error, not a silent default. That guard is the whole point of this
// test — the predicates are trivial today, but their split governs the
// sidebar "Waiting on you" vs "Running" buckets, so any drift here is a
// product regression.
const expected: Record<WaveLifecycle, { running: boolean; waiting: boolean }> =
  {
    draft: { running: false, waiting: false },
    planning: { running: true, waiting: false },
    dispatching: { running: true, waiting: false },
    working: { running: true, waiting: false },
    blocked: { running: false, waiting: true },
    reviewing: { running: false, waiting: true },
    done: { running: false, waiting: false },
    canceled: { running: false, waiting: false },
    failed: { running: false, waiting: true },
  };

describe('lifecycle predicates', () => {
  for (const [state, { running, waiting }] of Object.entries(expected) as [
    WaveLifecycle,
    { running: boolean; waiting: boolean },
  ][]) {
    it(`${state}: isRunning=${running}, isWaitingForUser=${waiting}`, () => {
      expect(isRunning(state)).toBe(running);
      expect(isWaitingForUser(state)).toBe(waiting);
    });
  }

  it('the two buckets are disjoint', () => {
    for (const state of Object.keys(expected) as WaveLifecycle[]) {
      expect(isRunning(state) && isWaitingForUser(state)).toBe(false);
    }
  });
});
