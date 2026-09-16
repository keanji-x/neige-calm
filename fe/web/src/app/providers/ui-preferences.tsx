import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useSyncExternalStore, type ReactNode } from 'react';

import { createStorageKey, DB_INSTANCE_ID_KEY } from '../../../../core/keys/storage.ts';
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
  let database: string | null = null;
  try { database = storage?.getItem(DB_INSTANCE_ID_KEY) ?? null; } catch { /* memory-only receipts */ }
  const notify = () => {
    revision += 1;
    for (const listener of listeners) listener();
  };
  const read = (key: string): Preference => {
    if (memory.has(key)) return memory.get(key) ?? null;
    let value: Preference = null;
    try {
      const parsed: unknown = JSON.parse(storage?.getItem(createStorageKey('ui', 'v1', encodeURIComponent(key))) ?? 'null');
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
      if (persist) storage?.setItem(createStorageKey('ui', 'v1', encodeURIComponent(key)), JSON.stringify(value));
    } catch {
      // The current app instance still remembers the choice when storage fails.
    }
    if (!notifyLayout) return;
    notify();
  };
  const receiptKey = (kind: 'track' | 'conversation', id: string) => `read:${database ?? 'local'}:${kind}:${id}`;
  const receipt = (key: string): number => {
    const stampOf = (value: unknown) => {
      const stamp = typeof value === 'string' ? Number(value) : 0;
      return Number.isFinite(stamp) && stamp > 0 ? stamp : 0;
    };
    if (database === null) return stampOf(memory.get(key));
    const cached = stampOf(read(key));
    try {
      // Re-read before acknowledging: another tab may already have seen a newer update.
      return Math.max(cached, stampOf(JSON.parse(storage?.getItem(createStorageKey('ui', 'v1', encodeURIComponent(key))) ?? 'null')));
    } catch { return cached; }
  };
  return Object.freeze({
    readScope: () => database,
    setReadScope(id: string | null): void {
      if (database === id) return;
      database = id;
      notify();
    },
    isUnread(kind: 'track' | 'conversation', id: string, updatedAt: number): boolean {
      return updatedAt > receipt(receiptKey(kind, id));
    },
    markRead(kind: 'track' | 'conversation', id: string, updatedAt: number): void {
      const key = receiptKey(kind, id);
      if (Number.isFinite(updatedAt) && updatedAt > receipt(key)) write(key, String(updatedAt), true, database !== null);
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
// Undefined means a standalone preferences host. Null means the compatibility
// gate has not yet confirmed which backend owns the visible data.
const ReadReceiptScopeContext = createContext<string | null | undefined>(undefined);

export function ReadReceiptScopeProvider({ id, children }: { id: string | null; children: ReactNode }) {
  return <ReadReceiptScopeContext.Provider value={id}>{children}</ReadReceiptScopeContext.Provider>;
}

export function UiPreferencesProvider({ preferences, children }: { preferences: UiPreferences; children: ReactNode }) {
  const instanceId = useContext(ReadReceiptScopeContext);
  useLayoutEffect(() => {
    if (instanceId !== undefined) preferences.setReadScope(instanceId);
  }, [instanceId, preferences]);
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
