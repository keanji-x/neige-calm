import { describe, expect, it } from 'vitest';
import { anchorRunningTurn } from './conversation-meta.js';

describe('anchorRunningTurn', () => {
  it('anchors the start at the receipt minus the elapsed time', () => {
    expect(anchorRunningTurn(null, { turn_id: 't1', elapsed_ms: 18_000 }, 100_000))
      .toEqual({ turnId: 't1', startMs: 82_000 });
  });

  it('keeps the earlier anchor when a later receipt of the same turn would move it later', () => {
    const first = anchorRunningTurn(null, { turn_id: 't1', elapsed_ms: 18_000 }, 100_000);
    // Two seconds of server time, but the response took three to arrive: the estimate shifts later.
    const later = anchorRunningTurn(first, { turn_id: 't1', elapsed_ms: 20_000 }, 103_000);
    expect(later).toBe(first);
  });

  it('takes an earlier anchor for the same turn, so the clock never runs backwards', () => {
    const first = anchorRunningTurn(null, { turn_id: 't1', elapsed_ms: 18_000 }, 100_000);
    expect(anchorRunningTurn(first, { turn_id: 't1', elapsed_ms: 21_000 }, 102_000))
      .toEqual({ turnId: 't1', startMs: 81_000 });
  });

  it('re-anchors on a new turn, even a later one', () => {
    const first = anchorRunningTurn(null, { turn_id: 't1', elapsed_ms: 18_000 }, 100_000);
    expect(anchorRunningTurn(first, { turn_id: 't2', elapsed_ms: 1_000 }, 200_000))
      .toEqual({ turnId: 't2', startMs: 199_000 });
  });

  it('clears when no turn is running', () => {
    const first = anchorRunningTurn(null, { turn_id: 't1', elapsed_ms: 18_000 }, 100_000);
    expect(anchorRunningTurn(first, null, 101_000)).toBeNull();
  });
});
