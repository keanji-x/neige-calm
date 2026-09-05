import { createStorageKey } from '../../../../core/keys/storage.ts';
import { parseWorkspaceRelativeFilePath } from '../../../../core/domain/report-file.ts';

const RECENT_FILE_LIMIT = 8;
const EMPTY_RECENT_FILES = Object.freeze([] as const);

export type RecentFileStorage = Pick<Storage, 'getItem' | 'setItem'>;

export type RecentFileHistory = Readonly<{
  read(trackId: string): readonly string[];
  record(trackId: string, path: string): readonly string[];
}>;

function storageKey(trackId: string) {
  return createStorageKey('recent-files', encodeURIComponent(trackId));
}

function decode(value: string | null): readonly string[] {
  if (value === null) return EMPTY_RECENT_FILES;
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return EMPTY_RECENT_FILES;
  }
  if (!Array.isArray(parsed)) return EMPTY_RECENT_FILES;
  const paths: string[] = [];
  for (const value of parsed) {
    if (typeof value !== 'string') continue;
    const target = parseWorkspaceRelativeFilePath(value);
    if (target === null || paths.includes(target.path)) continue;
    paths.push(target.path);
    if (paths.length === RECENT_FILE_LIMIT) break;
  }
  return Object.freeze(paths);
}

/**
 * Per-Track, browser-local MRU history. Storage failures degrade to memory for
 * the life of this app instance; opening a file must not depend on persistence.
 */
export function createRecentFileHistory(storage?: RecentFileStorage): RecentFileHistory {
  const memory = new Map<string, readonly string[]>();
  const read = (trackId: string): readonly string[] => {
    const current = memory.get(trackId);
    if (current !== undefined) return current;
    let restored: readonly string[] = EMPTY_RECENT_FILES;
    try {
      restored = decode(storage?.getItem(storageKey(trackId)) ?? null);
    } catch {
      // Browser storage may be disabled; the in-memory branch still works.
    }
    memory.set(trackId, restored);
    return restored;
  };
  return Object.freeze({
    read,
    record(trackId: string, path: string): readonly string[] {
      const target = parseWorkspaceRelativeFilePath(path);
      if (target === null) return read(trackId);
      const next = Object.freeze([
        target.path,
        ...read(trackId).filter((candidate) => candidate !== target.path),
      ].slice(0, RECENT_FILE_LIMIT));
      memory.set(trackId, next);
      try {
        storage?.setItem(storageKey(trackId), JSON.stringify(next));
      } catch {
        // Persistence is best-effort; the current app instance keeps `next`.
      }
      return next;
    },
  });
}
