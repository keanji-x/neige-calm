import { cleanup, renderHook, act, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import type { ReactNode } from 'react';
import { createUiPreferences, DatabaseScopeProvider, UiPreferencesProvider, useUiPreferences, type UiPreferences } from '../providers/ui-preferences.tsx';
import { useWorkspaceGuide } from './workspace-guide.tsx';

afterEach(cleanup);
function visit(preferences: UiPreferences, requested: boolean, ready = true) {
  return renderHook(() => useWorkspaceGuide(useUiPreferences(), requested, true, ready), {
    wrapper: ({ children }: { children: ReactNode }) => <UiPreferencesProvider preferences={preferences}>{children}</UiPreferencesProvider>,
  });
}

it('persists explanations read before a database identity is confirmed and restores them on a new visit', async () => {
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
  const preferences = createUiPreferences(storage);
  const view = visit(preferences, true);
  act(() => view.result.current.onSeen('workspace-add'));
  expect(view.result.current.isSeen('workspace-add')).toBe(true);
  expect(values.size).toBe(0);
  act(() => preferences.setDatabaseScope('confirmed'));
  await waitFor(() => expect(preferences.guideStepSeen(1, 'workspace-add')).toBe(true));
  view.unmount();
  const restored = createUiPreferences(storage); restored.setDatabaseScope('confirmed');
  expect(restored.guideStepSeen(1, 'workspace-add')).toBe(true);
});

it('commits a replay reset made while the database is unknown before using stored progress', async () => {
  const preferences = createUiPreferences(); preferences.setDatabaseScope('known');
  preferences.setGuideEnabled(true); preferences.setGuideStepSeen(1, 'workspace-add', true);
  const view = visit(preferences, false);
  expect(view.result.current.isSeen('workspace-add')).toBe(true);
  act(() => preferences.setDatabaseScope(null));
  act(() => view.result.current.start());
  expect(view.result.current.isSeen('workspace-add')).toBe(false);
  act(() => preferences.setDatabaseScope('known'));
  await waitFor(() => expect(preferences.guideStepSeen(1, 'workspace-add')).toBe(false));
  expect(view.result.current.isSeen('workspace-add')).toBe(false);
});

it('keeps visit progress separate when a confirmed database changes', async () => {
  const preferences = createUiPreferences(); preferences.setDatabaseScope('one');
  const view = visit(preferences, false);
  act(() => view.result.current.onSeen('workspace-add'));
  await waitFor(() => expect(preferences.guideStepSeen(1, 'workspace-add')).toBe(true));
  act(() => preferences.setDatabaseScope('two'));
  expect(view.result.current.isSeen('workspace-add')).toBe(false);
  expect(preferences.guideStepSeen(1, 'workspace-add')).toBe(false);
});


it('persists an explicit Skip even while workspace reads are unavailable', async () => {
  const preferences = createUiPreferences(); preferences.setDatabaseScope('offline'); preferences.setGuideEnabled(true);
  const view = visit(preferences, false, false);
  act(() => view.result.current.skip());
  await waitFor(() => expect(preferences.guideEnabled()).toBe(false));
});


it('keeps a cached database replay pending until the real provider confirms its scope', () => {
  const preferences = createUiPreferences(); preferences.setDatabaseScope('cached');
  preferences.setGuideEnabled(true); preferences.setGuideStepSeen(1, 'workspace-add', true);
  let scope: string | null = null;
  const view = renderHook(() => useWorkspaceGuide(useUiPreferences(), true, true, true), {
    wrapper: ({ children }: { children: ReactNode }) => <DatabaseScopeProvider id={scope}>
      <UiPreferencesProvider preferences={preferences}>{children}</UiPreferencesProvider>
    </DatabaseScopeProvider>,
  });
  expect(preferences.databaseScope()).toBe(null);
  scope = 'cached'; act(() => view.rerender());
  expect(preferences.databaseScope()).toBe('cached');
  expect(view.result.current.isSeen('workspace-add')).toBe(false);
});

it('rejects an old rendered progress plan when the scope provider changes during commit', () => {
  const preferences = createUiPreferences(); preferences.setDatabaseScope('one');
  let scope = 'one';
  const view = renderHook(() => useWorkspaceGuide(useUiPreferences(), false, true, true), {
    wrapper: ({ children }: { children: ReactNode }) => <DatabaseScopeProvider id={scope}>
      <UiPreferencesProvider preferences={preferences}>{children}</UiPreferencesProvider>
    </DatabaseScopeProvider>,
  });
  act(() => {
    view.result.current.onSeen('workspace-add');
    scope = 'two'; view.rerender();
  });
  expect(preferences.databaseScope()).toBe('two');
  expect(preferences.guideStepSeen(1, 'workspace-add')).toBe(false);
});

it.each(['seen', 'skip'] as const)('adopts unknown-period %s when first confirmation differs from cached identity', action => {
  const preferences = createUiPreferences(); preferences.setDatabaseScope('cached');
  preferences.setGuideEnabled(true);
  let scope: string | null = null;
  const view = renderHook(() => useWorkspaceGuide(useUiPreferences(), true, true, true), {
    wrapper: ({ children }: { children: ReactNode }) => <DatabaseScopeProvider id={scope}>
      <UiPreferencesProvider preferences={preferences}>{children}</UiPreferencesProvider>
    </DatabaseScopeProvider>,
  });
  act(() => { if (action === 'seen') view.result.current.onSeen('workspace-add'); else view.result.current.skip(); });
  scope = 'actual'; act(() => view.rerender());
  if (action === 'seen') expect(preferences.guideStepSeen(1, 'workspace-add')).toBe(true);
  else expect(preferences.guideEnabled()).toBe(false);
});
