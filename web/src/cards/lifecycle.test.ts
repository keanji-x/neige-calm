import { describe, expect, it, vi } from 'vitest';
import { createCardLifecycleStore } from './lifecycle';

describe('createCardLifecycleStore', () => {
  it('returns the default lifecycle snapshot', () => {
    const store = createCardLifecycleStore();

    expect(store.getSnapshot()).toEqual({
      visible: true,
      focused: false,
      geometry: { width: 0, height: 0, ready: false },
      refreshEpoch: 0,
    });
    expect(Object.isFrozen(store.getSnapshot())).toBe(true);
    expect(Object.isFrozen(store.getSnapshot().geometry)).toBe(true);
  });

  it('skips equality updates and notifies once on changes', () => {
    const store = createCardLifecycleStore();
    const listener = vi.fn();
    store.subscribe(listener);
    const firstSnapshot = store.getSnapshot();

    store.setVisible(true);
    store.setFocused(false);
    store.setGeometry({ width: 0, height: 0, ready: false });

    expect(listener).not.toHaveBeenCalled();
    expect(store.getSnapshot()).toBe(firstSnapshot);

    store.setVisible(false);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(store.getSnapshot()).not.toBe(firstSnapshot);
    const secondSnapshot = store.getSnapshot();

    store.setFocused(true);
    expect(listener).toHaveBeenCalledTimes(2);
    expect(store.getSnapshot()).not.toBe(secondSnapshot);
    const thirdSnapshot = store.getSnapshot();

    store.setGeometry({ width: 80, height: 24, ready: true });
    expect(listener).toHaveBeenCalledTimes(3);
    expect(store.getSnapshot()).not.toBe(thirdSnapshot);
  });

  it('increments refreshEpoch by one', () => {
    const store = createCardLifecycleStore();

    store.bumpRefresh();
    expect(store.getSnapshot().refreshEpoch).toBe(1);

    store.bumpRefresh();
    expect(store.getSnapshot().refreshEpoch).toBe(2);
  });

  it('unsubscribe removes the listener', () => {
    const store = createCardLifecycleStore();
    const listener = vi.fn();
    const off = store.subscribe(listener);

    off();
    store.setVisible(false);

    expect(listener).not.toHaveBeenCalled();
  });

  it('allows unsubscribe during listener iteration', () => {
    const store = createCardLifecycleStore();
    const second = vi.fn();
    let offFirst = () => {};
    const first = vi.fn(() => offFirst());
    offFirst = store.subscribe(first);
    store.subscribe(second);

    store.setVisible(false);

    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(1);
  });
});
