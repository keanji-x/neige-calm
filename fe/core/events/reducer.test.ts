import { describe, expect, it } from 'vitest';

import { decodeEventFrame } from './protocol.js';
import { initialEventState, reduceEventFrame } from './reducer.js';

function readyFrame(input: unknown) {
  const decoded = decodeEventFrame(input);
  if (decoded.status !== 'ready') throw new Error('fixture did not decode');
  return decoded.frame;
}

describe('event reducer behavior', () => {
  it('drops future-version frames without advancing the cursor', () => {
    const result = reduceEventFrame(
      { cursor: 9, syncEventVersion: 2 },
      readyFrame({ ev: 'cove.deleted', data: { id: 'c1' }, _id: 10, eventVersion: 3 }),
    );
    expect(result.state.cursor).toBe(9);
    expect(result.effects).toEqual([]);
  });

  it('advances cursor before rejecting an in-range malformed event', () => {
    const result = reduceEventFrame(
      { cursor: 9, syncEventVersion: 3 },
      readyFrame({ ev: 'cove.deleted', data: {}, _id: 10, eventVersion: 2 }),
    );
    expect(result.state.cursor).toBe(10);
    expect(result.effects).toEqual([{ type: 'persist-cursor', id: 10 }]);
  });

  it('turns control frames into pure cache and reconnect effects', () => {
    const replay = reduceEventFrame(
      { cursor: 20, syncEventVersion: 3 },
      readyFrame({ ev: '_replay_complete', _id: 19 }),
    );
    expect(replay.state.cursor).toBeNull();
    expect(replay.effects).toEqual([
      { type: 'persist-cursor', id: null },
      { type: 'clear-cache' },
      { type: 'reconnect' },
    ]);

    const snapshot = reduceEventFrame(initialEventState(3), readyFrame({ ev: '_snapshot_required' }));
    expect(snapshot.effects).toEqual([
      { type: 'persist-cursor', id: null },
      { type: 'clear-cache' },
      { type: 'reconnect' },
    ]);
  });

  it('converges normally completed replay with one global invalidation', () => {
    const replay = reduceEventFrame(
      { cursor: 20, syncEventVersion: 3 },
      readyFrame({ ev: '_replay_complete', _id: 20 }),
    );
    expect(replay.effects).toEqual([{ type: 'invalidate', keys: null }]);
  });

  it('advances a valid cursor and emits the event invalidation plan', () => {
    const result = reduceEventFrame(
      initialEventState(3),
      readyFrame({ ev: 'cove.deleted', data: { id: 'c1' }, _id: 7, eventVersion: 3 }),
    );
    expect(result.state.cursor).toBe(7);
    expect(result.effects).toEqual([
      { type: 'persist-cursor', id: 7 },
      { type: 'invalidate', keys: [['coves'], ['overlays', 'wave']] },
    ]);
  });

  it('never persists missing, non-positive, or non-increasing cursor ids', () => {
    for (const id of [undefined, 0, -1, 8]) {
      const result = reduceEventFrame(
        { cursor: 8, syncEventVersion: 3 },
        readyFrame({ ev: 'cove.deleted', data: { id: 'c1' }, _id: id, eventVersion: 3 }),
      );
      expect(result.state.cursor).toBe(8);
      expect(result.effects).toEqual([{ type: 'invalidate', keys: [['coves'], ['overlays', 'wave']] }]);
    }
  });
});
