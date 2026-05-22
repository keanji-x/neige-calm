// Tests for `web/src/api/onUnauthorized.ts` (issue #189).
//
// The module is a tiny event bus: SessionProvider subscribes via
// `onUnauthorized()`, and calm.ts + events.ts fire via
// `fireUnauthorized()`. We verify:
//
//   * a fired event reaches every subscribed listener,
//   * unsubscribe stops further deliveries,
//   * a thrown listener doesn't kill the others (calm.ts must not be
//     stalled by a buggy subscriber),
//   * the test-only reset clears state between tests.

import { describe, it, expect, vi, beforeEach } from 'vitest';
import {
  fireUnauthorized,
  onUnauthorized,
  _resetUnauthorizedListenersForTest,
} from './onUnauthorized';

beforeEach(() => {
  _resetUnauthorizedListenersForTest();
});

async function flushMicrotasks() {
  // `fireUnauthorized` dispatches via `queueMicrotask`. Awaiting one
  // resolved promise drains the queue in the same task tick.
  await Promise.resolve();
}

describe('onUnauthorized / fireUnauthorized', () => {
  it('delivers to every subscribed listener', async () => {
    const a = vi.fn();
    const b = vi.fn();
    onUnauthorized(a);
    onUnauthorized(b);
    fireUnauthorized();
    await flushMicrotasks();
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1);
  });

  it('unsubscribe stops further deliveries', async () => {
    const a = vi.fn();
    const off = onUnauthorized(a);
    fireUnauthorized();
    await flushMicrotasks();
    off();
    fireUnauthorized();
    await flushMicrotasks();
    expect(a).toHaveBeenCalledTimes(1);
  });

  it('isolates listener errors — other subscribers still fire', async () => {
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const a = vi.fn(() => {
      throw new Error('boom');
    });
    const b = vi.fn();
    onUnauthorized(a);
    onUnauthorized(b);
    fireUnauthorized();
    await flushMicrotasks();
    expect(b).toHaveBeenCalledTimes(1);
    errSpy.mockRestore();
  });
});
