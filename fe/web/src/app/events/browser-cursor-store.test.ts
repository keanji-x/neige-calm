import { describe, expect, it, vi } from 'vitest';

import { SYNC_CURSOR_KEY } from '../../../../core/keys/storage.ts';
import { createBrowserCursorStore, type CursorIdleScheduler } from './browser-cursor-store.ts';

function memoryStorage(initial?: string) {
  const values = new Map<string, string>();
  if (initial !== undefined) values.set(SYNC_CURSOR_KEY, initial);
  return {
    values,
    getItem: vi.fn((key: string) => values.get(key) ?? null),
    setItem: vi.fn((key: string, value: string) => { values.set(key, value); }),
    removeItem: vi.fn((key: string) => { values.delete(key); }),
  };
}

function controlledIdle() {
  const callbacks: Array<() => void> = [];
  const cancelled = new Set<number>();
  const idle: CursorIdleScheduler = {
    schedule: vi.fn<(callback: () => void) => number>((callback) => { callbacks.push(callback); return callbacks.length; }),
    cancel: vi.fn((handle) => { cancelled.add(handle as number); }),
  };
  return {
    idle,
    drainActive: () => callbacks.forEach((callback, index) => { if (!cancelled.has(index + 1)) callback(); }),
    drainIncludingCancelled: () => callbacks.forEach((callback) => callback()),
  };
}

describe('browser cursor store', () => {
  it('T-C1 write is synchronously visible from memory', () => {
    const store = createBrowserCursorStore(memoryStorage(), controlledIdle().idle);
    store.adopt('db-a'); store.write(7);
    expect(store.read()).toBe(7);
  });

  it('T-C2 batches writes into one idle flush carrying the latest cursor', () => {
    const storage = memoryStorage(); const idle = controlledIdle();
    const store = createBrowserCursorStore(storage, idle.idle); store.adopt('db-a');
    for (let cursor = 1; cursor <= 5; cursor += 1) store.write(cursor);
    expect(idle.idle.schedule).toHaveBeenCalledOnce(); expect(storage.setItem).not.toHaveBeenCalled();
    idle.drainActive();
    expect(storage.setItem).toHaveBeenCalledOnce();
    expect(JSON.parse(storage.values.get(SYNC_CURSOR_KEY)!)).toEqual({ dbInstanceId: 'db-a', cursor: 5 });
  });

  it('T-C3 clear prevents even an already-dequeued idle callback from restoring a cursor', () => {
    const storage = memoryStorage(); const idle = controlledIdle();
    const store = createBrowserCursorStore(storage, idle.idle); store.adopt('db-a'); store.write(7); store.clear();
    idle.drainIncludingCancelled();
    expect(storage.getItem(SYNC_CURSOR_KEY)).toBeNull();
  });

  it('T-C4 rejects a persisted cursor stamped for another database instance', () => {
    const storage = memoryStorage(JSON.stringify({ dbInstanceId: 'db-old', cursor: 12 }));
    const store = createBrowserCursorStore(storage, controlledIdle().idle); store.adopt('db-new');
    expect(store.read()).toBeNull();
  });

  it('T-C5 contains storage failures without losing the in-memory cursor', () => {
    const storage = { getItem: () => null, removeItem: () => undefined, setItem: () => { throw new Error('blocked'); } };
    const idle = controlledIdle(); const store = createBrowserCursorStore(storage, idle.idle);
    store.adopt('db-a'); store.write(9);
    expect(() => idle.drainActive()).not.toThrow(); expect(store.read()).toBe(9);
  });

  it('T-C6 fails closed before adopt and does not persist pre-adopt writes', () => {
    const storage = memoryStorage(JSON.stringify({ dbInstanceId: 'db-a', cursor: 4 })); const idle = controlledIdle();
    const store = createBrowserCursorStore(storage, idle.idle);
    expect(store.read()).toBeNull(); store.write(8); idle.drainActive();
    expect(storage.setItem).not.toHaveBeenCalled(); expect(store.read()).toBeNull();
  });

  it('T-C7 rejects the legacy web bare-number value without throwing', () => {
    const store = createBrowserCursorStore(memoryStorage('123'), controlledIdle().idle);
    expect(() => store.adopt('db-a')).not.toThrow(); expect(store.read()).toBeNull();
  });

  it('clear keeps the adopted instance so the next write can flush', () => {
    const storage = memoryStorage(); const idle = controlledIdle();
    const store = createBrowserCursorStore(storage, idle.idle); store.adopt('db-a'); store.clear(); store.write(11); idle.drainActive();
    expect(JSON.parse(storage.values.get(SYNC_CURSOR_KEY)!)).toEqual({ dbInstanceId: 'db-a', cursor: 11 });
  });

  it('a null write durably resets an adopted cursor', () => {
    const storage = memoryStorage(JSON.stringify({ dbInstanceId: 'db-a', cursor: 6 })); const idle = controlledIdle();
    const store = createBrowserCursorStore(storage, idle.idle); store.adopt('db-a'); store.write(null); idle.drainIncludingCancelled();
    expect(store.read()).toBeNull(); expect(storage.getItem(SYNC_CURSOR_KEY)).toBeNull();
  });
});
