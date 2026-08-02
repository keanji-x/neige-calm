import { describe, expect, it, vi } from 'vitest';

import { createUnauthorizedChannel } from './unauthorized.js';

describe('core/api unauthorized channel contract', () => {
  it('defers dispatch and isolates failing listeners', () => {
    const pending: Array<() => void> = [];
    const calls: string[] = [];
    const report = vi.fn();
    const channel = createUnauthorizedChannel(
      { enqueue: (task) => pending.push(task) },
      { report },
    );
    channel.subscribe(() => { calls.push('first'); throw new Error('broken listener'); });
    channel.subscribe(() => { calls.push('second'); });

    channel.notify();
    expect(calls).toEqual([]);
    expect(pending).toHaveLength(1);
    pending[0]?.();
    expect(calls).toEqual(['first', 'second']);
    expect(report).toHaveBeenCalledTimes(1);
  });

  it('returns an unsubscribe operation owned by the channel instance', () => {
    const pending: Array<() => void> = [];
    const listener = vi.fn();
    const channel = createUnauthorizedChannel({ enqueue: (task) => pending.push(task) });
    const unsubscribe = channel.subscribe(listener);
    unsubscribe();
    channel.notify();
    pending[0]?.();
    expect(listener).not.toHaveBeenCalled();
  });
});
