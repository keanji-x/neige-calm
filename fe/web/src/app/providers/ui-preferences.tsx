import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useSyncExternalStore, type ReactNode } from 'react';

import { createStorageKey, DATABASE_ID_KEY } from '../../../../core/keys/storage.ts';
import { useState } from '../../ui/state/public.ts';

type Preference = boolean | string | null;
export type UiPreferenceStorage = Pick<Storage, 'getItem' | 'setItem'>;
export type UiPreferences = ReturnType<typeof createUiPreferences>;

/** Display preferences only. Each entity has its own versioned key so writes
 * in different tabs cannot replace other Tracks' or Areas' preferences. */
export function createUiPreferences(storage?: UiPreferenceStorage) {
  const memory = new Map<string, Preference>();
  const listeners = new Set<() => void>();
  let revision = 0;
  /* Seeded from the database's STABLE identity, never the per-boot `DB_INSTANCE_ID_KEY`: receipts keyed
   * by a process id were thrown away on every kernel restart. `null` until the compat gate confirms it. */
  let database: string | null = null;
  try { database = storage?.getItem(DATABASE_ID_KEY) ?? null; } catch { /* memory-only receipts */ }
  const notify = () => {
    revision += 1;
    for (const listener of listeners) listener();
  };
  let recoveryScope: string | null = null;
  /* Display preferences live under the bundled recovery scope `[origin, userId, dbInstanceId]`, but receipts
   * and the baseline (`read:*`) must NOT: `dbInstanceId` is per boot, so they use origin and userId only. */
  let receiptNamespace: readonly [origin: string, userId: string] | null = null;
  const receiptNamespaceOf = (scope: string): readonly [string, string] | null => {
    try {
      const parsed: unknown = JSON.parse(scope);
      if (Array.isArray(parsed) && parsed.length === 3
        && parsed.every((part) => typeof part === 'string' && part.length > 0)) {
        return [parsed[0] as string, parsed[1] as string];
      }
    } catch { /* not the bundled triple */ }
    return null;
  };
  const storageKey = (key: string) => {
    if (recoveryScope === null) return createStorageKey('ui', 'v1', encodeURIComponent(key));
    if (receiptNamespace !== null && key.startsWith('read:')) {
      return createStorageKey('ui', 'receipts',
        encodeURIComponent(receiptNamespace[0]), encodeURIComponent(receiptNamespace[1]), encodeURIComponent(key));
    }
    return createStorageKey('ui', 'recovery', encodeURIComponent(recoveryScope), encodeURIComponent(key));
  };
  const read = (key: string): Preference => {
    if (memory.has(key)) return memory.get(key) ?? null;
    let value: Preference = null;
    try {
      const parsed: unknown = JSON.parse(storage?.getItem(storageKey(key)) ?? 'null');
      if (typeof parsed === 'boolean' || (typeof parsed === 'string' && parsed.length > 0)) value = parsed;
    } catch {
      // Malformed or unavailable storage leaves the default display usable.
    }
    memory.set(key, value);
    return value;
  };
  const write = (key: string, value: Preference, notifyLayout: boolean, persist = true) => {
    if ((persist ? read(key) : memory.get(key)) === value) return;
    memory.set(key, value);
    try {
      if (persist) storage?.setItem(storageKey(key), JSON.stringify(value));
    } catch {
      // The current app instance still remembers the choice when storage fails.
    }
    if (!notifyLayout) return;
    notify();
  };
  const receiptKey = (kind: 'track' | 'conversation', id: string) => `read:${database ?? 'local'}:${kind}:${id}`;
  /* A device's first entry into a database scope marks everything read: the baseline is written once per
   * `(device, databaseId)` with the SERVER's clock, and every receipt reads as at least that. */
  const baselineKey = (databaseId: string) => `read:${databaseId}:baseline`;
  const stampOf = (value: unknown) => {
    const stamp = typeof value === 'string' ? Number(value) : 0;
    return Number.isFinite(stamp) && stamp > 0 ? stamp : 0;
  };
  const receipt = (key: string): number => {
    if (database === null) return stampOf(memory.get(key));
    const baseline = stampOf(read(baselineKey(database)));
    const cached = stampOf(read(key));
    try {
      // Re-read before acknowledging: another tab may already have seen a newer update.
      return Math.max(baseline, cached, stampOf(JSON.parse(storage?.getItem(storageKey(key)) ?? 'null')));
    } catch { return Math.max(baseline, cached); }
  };
  return Object.freeze({
    readScope: () => database,
    /** `nowMs` is the server time the scope was confirmed at; it becomes the baseline the first time this device sees `id`. A `null` scope writes nothing. */
    setReadScope(id: string | null, nowMs: number | null = null): void {
      if (id !== null && nowMs !== null && Number.isFinite(nowMs) && nowMs > 0 && read(baselineKey(id)) === null) {
        write(baselineKey(id), String(nowMs), false);
      }
      if (database === id) return;
      database = id;
      notify();
    },
    isUnread(kind: 'track' | 'conversation', id: string, updatedAt: number): boolean {
      // No scope, no verdict: "everything is unread until /api/version answers" is a lie on every page load.
      if (database === null) return false;
      return updatedAt > receipt(receiptKey(kind, id));
    },
    markRead(kind: 'track' | 'conversation', id: string, updatedAt: number): void {
      const key = receiptKey(kind, id);
      if (Number.isFinite(updatedAt) && updatedAt > receipt(key)) write(key, String(updatedAt), true, database !== null);
    },
    setRecoveryScope(scope: string): void {
      if (recoveryScope === scope) return;
      recoveryScope = scope; receiptNamespace = receiptNamespaceOf(scope); memory.clear(); notify();
    },
    // Only layout changes are live: conversation selection is restored on route
    // entry and owned by React while open, so saving it must not move focus.
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
    getSnapshot: () => revision,
    areaExpanded(id: string): boolean {
      const value = read(`area:${id}`);
      return typeof value === 'boolean' ? value : true;
    },
    setAreaExpanded: (id: string, value: boolean) => write(`area:${id}`, value, true),
    conversation(id: string): string | null {
      const value = read(`conversation:${id}`);
      return typeof value === 'string' ? value : null;
    },
    setConversation: (id: string, value: string | null) => write(`conversation:${id}`, value, false),
    railCollapsed(): boolean | null {
      const value = read('rail-collapsed');
      return typeof value === 'boolean' ? value : null;
    },
    setRailCollapsed: (value: boolean) => write('rail-collapsed', value, true),
  });
}

const UiPreferencesContext = createContext<UiPreferences | null>(null);
/** The database the visible data belongs to, and the server time that was confirmed at. */
export type ReadReceiptScope = Readonly<{ id: string | null; nowMs: number | null }>;
// Undefined means a standalone preferences host; a null `id` means the gate has not confirmed the database
// yet (or the server predates database identity), in which case nothing is ever unread.
const ReadReceiptScopeContext = createContext<ReadReceiptScope | undefined>(undefined);

export function ReadReceiptScopeProvider({ id, nowMs = null, children }: { id: string | null; nowMs?: number | null; children: ReactNode }) {
  const scope = useMemo<ReadReceiptScope>(() => ({ id, nowMs }), [id, nowMs]);
  return <ReadReceiptScopeContext.Provider value={scope}>{children}</ReadReceiptScopeContext.Provider>;
}

export function UiPreferencesProvider({ preferences, children }: { preferences: UiPreferences; children: ReactNode }) {
  const scope = useContext(ReadReceiptScopeContext);
  // Layout, not passive: the baseline has to be on disk before the first
  // `useReadReceipt` (a passive effect in the same commit) persists a receipt.
  useLayoutEffect(() => {
    if (scope !== undefined) preferences.setReadScope(scope.id, scope.nowMs);
  }, [scope, preferences]);
  return <UiPreferencesContext.Provider value={preferences}>{children}</UiPreferencesContext.Provider>;
}

export function useUiPreferences(): UiPreferences {
  const context = useContext(UiPreferencesContext);
  const [local] = useState(() => createUiPreferences());
  const preferences = context ?? local;
  useSyncExternalStore(preferences.subscribe, preferences.getSnapshot, preferences.getSnapshot);
  return preferences;
}

type OpenTarget = Readonly<{ kind: 'row'; id: string } | { kind: 'draft' }>;

/** Only existing conversation IDs are persisted. Drafts keep their existing
 * provider lifecycle, and restoration must still resolve through live rows. */
export function useConversationViewTarget(scopeId: string) {
  const preferences = useUiPreferences();
  const restore = (): OpenTarget | null => {
    const id = preferences.conversation(scopeId);
    return id === null ? null : { kind: 'row', id };
  };
  const [selection, setSelection] = useState(() => ({ scopeId, target: restore() }));
  const current = selection.scopeId === scopeId ? selection : { scopeId, target: restore() };
  if (current !== selection) setSelection(current);
  const setTarget = useCallback((target: OpenTarget | null) => {
    setSelection({ scopeId, target });
  }, [scopeId]);
  const { scopeId: currentScopeId, target } = current;
  useEffect(() => {
    // Write after the render so opening and composer-focus intent commit together.
    preferences.setConversation(currentScopeId, target?.kind === 'row' ? target.id : null);
  }, [preferences, currentScopeId, target]);
  return [current.target, setTarget] as const;
}

/** A background tab never acknowledges work the reader has not seen. */
export function useReadReceipt(kind: 'track' | 'conversation', id: string | null, updatedAt: number, enabled = true) {
  const preferences = useUiPreferences();
  const scope = preferences.readScope();
  useEffect(() => {
    if (id === null || !enabled) return;
    const markVisible = () => {
      if (document.visibilityState === 'visible') preferences.markRead(kind, id, updatedAt);
    };
    markVisible();
    document.addEventListener('visibilitychange', markVisible);
    return () => document.removeEventListener('visibilitychange', markVisible);
  }, [preferences, scope, kind, id, updatedAt, enabled]);
}
