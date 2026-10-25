import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useSyncExternalStore, type ReactNode } from 'react';

import { createStorageKey, DATABASE_ID_KEY } from '../../../../core/keys/storage.ts';
import { sidebarOrder, type SidebarGroupId, type SidebarOrderScope } from '../../../../core/view/sidebar-layout.ts';
import { useState } from '../../ui/state/public.ts';

export type GuideProgressCommit = Readonly<{
  version: number; reset: readonly string[]; seen: readonly string[]; enabled: boolean | null;
}>;
function guideEnabledKey(database: string): string { return `browser:guide-enabled:${database}`; }
function guideStepKey(database: string, version: number, id: string): string { return `browser:guide-step:${database}:${version}:${id}`; }

type Preference = boolean | string | null;
export type UiPreferenceStorage = Pick<Storage, 'getItem' | 'setItem'>;
export type UiPreferences = ReturnType<typeof createUiPreferences>;

/** Display preferences only. Each entity has its own versioned key so writes
 * in different tabs cannot replace other Tracks' or Areas' preferences. */
export function createUiPreferences(storage?: UiPreferenceStorage) {
  const memory = new Map<string, Preference>();
  const listeners = new Set<() => void>();
  let revision = 0;
  /* Seeded from the database's STABLE identity, never the per-boot `DB_INSTANCE_ID_KEY`: choices keyed
   * by a process id were thrown away on every kernel restart. `null` until the compat gate confirms it. */
  let database: string | null = null;
  try { database = storage?.getItem(DATABASE_ID_KEY) ?? null; } catch { /* memory-only choices */ }
  const notify = () => {
    revision += 1;
    for (const listener of listeners) listener();
  };
  let recoveryScope: string | null = null;
  /* Display preferences live under the bundled recovery scope `[origin, userId, dbInstanceId]`, but per-browser
   * choices (`browser:*`) must NOT: `dbInstanceId` is per boot, so they use origin and userId only. */
  let browserNamespace: readonly [origin: string, userId: string] | null = null;
  const browserNamespaceOf = (scope: string): readonly [string, string] | null => {
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
    /* A per-browser display choice outlives a kernel restart: origin and user, not the boot. */
    if (browserNamespace !== null && key.startsWith('browser:')) {
      return createStorageKey('ui', 'browser',
        encodeURIComponent(browserNamespace[0]), encodeURIComponent(browserNamespace[1]), encodeURIComponent(key));
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
  const write = (key: string, value: Preference, notifyLayout: boolean) => {
    if (read(key) === value) return;
    memory.set(key, value);
    try {
      storage?.setItem(storageKey(key), JSON.stringify(value));
    } catch {
      // The current app instance still remembers the choice when storage fails.
    }
    if (!notifyLayout) return;
    notify();
  };
  const guideScope = () => database === null ? null : storageKey(`browser:guide:${database}`);
  return Object.freeze({
    databaseScope: () => database,
    /** The database the visible data belongs to; a `null` scope keeps no per-database choice. */
    setDatabaseScope(id: string | null): void {
      if (database === id) return;
      database = id;
      notify();
    },
    areaTrackPinned(areaId: string, trackId: string): boolean {
      if (database === null) return false;
      return read(`browser:area-pin:${database}:${areaId}:${trackId}`) === true;
    },
    setAreaTrackPinned(areaId: string, trackId: string, pinned: boolean): void {
      if (database !== null) write(`browser:area-pin:${database}:${areaId}:${trackId}`, pinned, true);
    },
    setRecoveryScope(scope: string): void {
      if (recoveryScope === scope) return;
      recoveryScope = scope; browserNamespace = browserNamespaceOf(scope); memory.clear(); notify();
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
    /** The Area's rail shows its closed tracks too; off by default. */
    areaShowsClosed(id: string): boolean {
      return read(`area-closed:${id}`) === true;
    },
    setAreaShowsClosed: (id: string, value: boolean) => write(`area-closed:${id}`, value, true),
    /** Sidebar groups use stable browser choices; Area disclosures retain their existing keys. */
    sidebarGroupExpanded(id: string): boolean {
      return read(`browser:sidebar-group:${id}`) !== false;
    },
    setSidebarGroupExpanded: (id: string, value: boolean) => write(`browser:sidebar-group:${id}`, value, true),
    sidebarGroupVisible(id: SidebarGroupId): boolean {
      const value = read(`browser:sidebar-visible:${id}`);
      return typeof value === 'boolean' ? value : id !== 'unread' && id !== 'running';
    },
    setSidebarGroupVisible: (id: SidebarGroupId, value: boolean) => write(`browser:sidebar-visible:${id}`, value, true),
    sidebarOrder<T extends string>(scope: SidebarOrderScope, ids: readonly T[]): T[] {
      let saved: unknown = null;
      try { saved = JSON.parse(String(read(`browser:sidebar-order:${scope}`))); } catch { /* use registered order */ }
      return sidebarOrder(ids, saved);
    },
    setSidebarOrder: (scope: SidebarOrderScope, ids: readonly string[]) => write(`browser:sidebar-order:${scope}`, JSON.stringify(ids), true),
    /** Optional guide display progress follows the stable database, never a process boot. */
    guideEnabled(): boolean | null {
      if (database === null) return null;
      const value = read(guideEnabledKey(database));
      return typeof value === 'boolean' ? value : null;
    },
    setGuideEnabled(enabled: boolean): void {
      if (database !== null) write(guideEnabledKey(database), enabled, true);
    },
    guideStepSeen(version: number, id: string): boolean {
      return database !== null && read(guideStepKey(database, version, id)) === true;
    },
    setGuideStepSeen(version: number, id: string, seen: boolean): void {
      if (database !== null) write(guideStepKey(database, version, id), seen, true);
    },
    /** Opaque stable identity for this display namespace; null while identity is unconfirmed. */
    guideScope,
    commitGuideProgress(expectedScope: string, progress: GuideProgressCommit): boolean {
      if (database === null || guideScope() !== expectedScope) return false;
      let changed = false;
      const stage = (key: string, value: boolean) => {
        if (read(key) === value) return;
        write(key, value, false);
        changed = true;
      };
      // No layout notification between fields: all keys belong to this same accepted scope.
      for (const id of progress.reset) stage(guideStepKey(database, progress.version, id), false);
      for (const id of progress.seen) stage(guideStepKey(database, progress.version, id), true);
      if (progress.enabled !== null) stage(guideEnabledKey(database), progress.enabled);
      if (changed) notify();
      return guideScope() === expectedScope;
    },
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
    /** The conversation drawer's dragged width in rem, or `null` for the default; stored as a string. */
    drawerWidth(): number | null {
      const stored = read('browser:drawer-width');
      const value = typeof stored === 'string' ? Number(stored) : 0;
      return Number.isFinite(value) && value > 0 ? value : null;
    },
    setDrawerWidth: (rem: number | null) => write('browser:drawer-width', rem === null ? null : String(rem), true),
    /** A report preview block's device choice (#1780), as the block serialized it; per Track and block key. */
    previewViewport(trackId: string, key: string): string | null {
      const value = read(`preview-viewport:${trackId}:${key}`);
      return typeof value === 'string' ? value : null;
    },
    setPreviewViewport: (trackId: string, key: string, value: string) => write(`preview-viewport:${trackId}:${key}`, value, false),
  });
}

const UiPreferencesContext = createContext<UiPreferences | null>(null);
// The database the visible data belongs to. Undefined means a standalone preferences host; `null` means the
// gate has not confirmed the database yet, in which case no per-database choice (an Area pin) applies.
const DatabaseScopeContext = createContext<string | null | undefined>(undefined);

export function DatabaseScopeProvider({ id, children }: { id: string | null; children: ReactNode }) {
  return <DatabaseScopeContext.Provider value={id}>{children}</DatabaseScopeContext.Provider>;
}

export function UiPreferencesProvider({ preferences, children }: { preferences: UiPreferences; children: ReactNode }) {
  const scope = useContext(DatabaseScopeContext);
  // Layout, not passive: rows never paint their Area pins under a scope the gate has replaced.
  useLayoutEffect(() => {
    if (scope !== undefined) preferences.setDatabaseScope(scope);
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
