import { createContext, useCallback, useContext, useEffect, useSyncExternalStore, type ReactNode } from 'react';

import { createStorageKey } from '../../../../core/keys/storage.ts';
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
  const write = (key: string, value: Preference, notifyLayout: boolean) => {
    if (read(key) === value) return;
    memory.set(key, value);
    try {
      storage?.setItem(createStorageKey('ui', 'v1', encodeURIComponent(key)), JSON.stringify(value));
    } catch {
      // The current app instance still remembers the choice when storage fails.
    }
    if (!notifyLayout) return;
    revision += 1;
    for (const listener of listeners) listener();
  };
  return Object.freeze({
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

export function UiPreferencesProvider({ preferences, children }: { preferences: UiPreferences; children: ReactNode }) {
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
