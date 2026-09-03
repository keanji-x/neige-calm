import { describe, expect, it } from 'vitest';

import type { Track, TrackLifecycle } from '../types';
import {
  isRunning,
  isWaitingForUser,
  sortByLifecycleRank,
  trackNeedsUserAttention,
} from './lifecycle';

// Exhaustive mapping pinned to a `Record<TrackLifecycle, ...>` literal:
// adding a new variant to `TrackLifecycle` without a row here is a type
// error, not a silent default. That guard is the whole point of this
// test — the predicates are trivial today, but their split governs the
// sidebar "Waiting on you" vs "Running" buckets, so any drift here is a
// product regression.
const expected: Record<TrackLifecycle, { running: boolean; waiting: boolean }> =
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
    TrackLifecycle,
    { running: boolean; waiting: boolean },
  ][]) {
    it(`${state}: isRunning=${running}, isWaitingForUser=${waiting}`, () => {
      expect(isRunning(state)).toBe(running);
      expect(isWaitingForUser(state)).toBe(waiting);
    });
  }

  it('the two buckets are disjoint', () => {
    for (const state of Object.keys(expected) as TrackLifecycle[]) {
      expect(isRunning(state) && isWaitingForUser(state)).toBe(false);
    }
  });
});

// Issue #254 — `trackNeedsUserAttention` ORs the pure-lifecycle bucket
// with the kernel-derived `anyCardNeedsInput`. The 4-cell matrix below
// pins the truth table exhaustively: `false` only when BOTH inputs are
// false; `true` otherwise. The base-track fixture uses a lifecycle that
// is NOT in the `isWaitingForUser` set (`draft` → returns false) so the
// false-base cases really do exercise the OR.
function fixture(overrides: Partial<Track>): Track {
  return {
    id: 'w1',
    areaId: 'c1',
    title: 't',
    lifecycle: 'draft',
    anyCardNeedsInput: false,
    progress: 0,
    eta: '',
    now: '',
    // Issue #250 PR 5 — required Track fields; unused by the
    // trackNeedsUserAttention matrix but the typechecker insists.
    createdAt: 0,
    terminalAt: null,
    pinnedAt: null,
    ...overrides,
  };
}

type Cell = {
  lifecycle: TrackLifecycle;
  anyCardNeedsInput: boolean;
  expected: boolean;
};

// 2×2 matrix pinned to a Record so a future fifth row (or a forgotten
// flip) is a type / compile error rather than a silent product
// regression.
const matrix: Record<string, Cell> = {
  'lifecycle=draft, anyCardNeedsInput=false → false': {
    lifecycle: 'draft',
    anyCardNeedsInput: false,
    expected: false,
  },
  'lifecycle=draft, anyCardNeedsInput=true → true (card-level signal alone)': {
    lifecycle: 'draft',
    anyCardNeedsInput: true,
    expected: true,
  },
  'lifecycle=blocked, anyCardNeedsInput=false → true (lifecycle alone)': {
    lifecycle: 'blocked',
    anyCardNeedsInput: false,
    expected: true,
  },
  'lifecycle=reviewing, anyCardNeedsInput=true → true (both)': {
    lifecycle: 'reviewing',
    anyCardNeedsInput: true,
    expected: true,
  },
};

describe('trackNeedsUserAttention', () => {
  for (const [name, { lifecycle, anyCardNeedsInput, expected }] of Object.entries(
    matrix,
  )) {
    it(name, () => {
      expect(
        trackNeedsUserAttention(fixture({ lifecycle, anyCardNeedsInput })),
      ).toBe(expected);
    });
  }
});

describe('sortByLifecycleRank', () => {
  it('orders waiting, then running, then other while preserving input order within buckets', () => {
    const tracks = [
      fixture({ id: 'done-1', lifecycle: 'done' }),
      fixture({ id: 'running-1', lifecycle: 'planning' }),
      fixture({ id: 'waiting-1', lifecycle: 'blocked' }),
      fixture({ id: 'other-1', lifecycle: 'draft' }),
      fixture({ id: 'running-2', lifecycle: 'working' }),
      fixture({ id: 'waiting-2', lifecycle: 'failed' }),
    ];

    expect(sortByLifecycleRank(tracks).map((w) => w.id)).toEqual([
      'waiting-1',
      'waiting-2',
      'running-1',
      'running-2',
      'done-1',
      'other-1',
    ]);
    expect(tracks.map((w) => w.id)).toEqual([
      'done-1',
      'running-1',
      'waiting-1',
      'other-1',
      'running-2',
      'waiting-2',
    ]);
  });
});
