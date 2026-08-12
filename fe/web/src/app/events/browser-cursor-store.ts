import { SYNC_CURSOR_KEY } from '../../../../core/keys/storage.ts';
import type { SyncCursorPort } from '../../systems/events/cursor-port.ts';

type CursorStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
type IdleHandle = ReturnType<typeof setTimeout> | number;

export type CursorIdleScheduler = Readonly<{
  schedule(callback: () => void): IdleHandle;
  cancel(handle: IdleHandle): void;
}>;

function defaultIdleScheduler(): CursorIdleScheduler {
  if (typeof window !== 'undefined' && typeof window.requestIdleCallback === 'function') {
    return {
      schedule: (callback) => window.requestIdleCallback(callback),
      cancel: (handle) => window.cancelIdleCallback(handle as number),
    };
  }
  return {
    schedule: (callback) => setTimeout(callback, 0),
    cancel: (handle) => clearTimeout(handle),
  };
}

function persistedCursor(raw: string | null, dbInstanceId: string): number | null {
  if (raw === null) return null;
  try {
    const value: unknown = JSON.parse(raw);
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return null;
    const record = value as Record<string, unknown>;
    if (record.dbInstanceId !== dbInstanceId) return null;
    return typeof record.cursor === 'number' && Number.isSafeInteger(record.cursor) && record.cursor >= 0
      ? record.cursor : null;
  } catch {
    return null;
  }
}

export function createBrowserCursorStore(
  storage: CursorStorage,
  idle: CursorIdleScheduler = defaultIdleScheduler(),
): SyncCursorPort {
  let adoptedInstanceId: string | null = null;
  let cursor: number | null = null;
  let writtenBeforeAdopt = false;
  let idleHandle: IdleHandle | null = null;

  const flush = (): void => {
    idleHandle = null;
    if (adoptedInstanceId === null || cursor === null) return;
    try {
      storage.setItem(SYNC_CURSOR_KEY, JSON.stringify({ dbInstanceId: adoptedInstanceId, cursor }));
    } catch { /* persistence is best-effort; memory remains authoritative */ }
  };

  return {
    read: () => adoptedInstanceId === null ? null : cursor,
    write: (value) => {
      cursor = value;
      if (adoptedInstanceId === null) {
        writtenBeforeAdopt = true;
        return;
      }
      if (value === null) {
        if (idleHandle !== null) idle.cancel(idleHandle);
        idleHandle = null;
        try { storage.removeItem(SYNC_CURSOR_KEY); } catch { /* persistence is best-effort */ }
        return;
      }
      if (idleHandle === null) idleHandle = idle.schedule(flush);
    },
    adopt: (dbInstanceId) => {
      const changed = adoptedInstanceId !== dbInstanceId;
      adoptedInstanceId = dbInstanceId;
      if (writtenBeforeAdopt) {
        writtenBeforeAdopt = false;
        return;
      }
      if (!changed) return;
      try { cursor = persistedCursor(storage.getItem(SYNC_CURSOR_KEY), dbInstanceId); }
      catch { cursor = null; }
    },
    clear: () => {
      if (idleHandle !== null) idle.cancel(idleHandle);
      idleHandle = null;
      cursor = null;
      writtenBeforeAdopt = false;
      try { storage.removeItem(SYNC_CURSOR_KEY); } catch { /* persistence is best-effort */ }
    },
  };
}
