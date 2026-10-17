import { cleanup } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { DATABASE_ID_KEY, DB_INSTANCE_ID_KEY } from '../../../../core/keys/storage.ts';
import { createUiPreferences } from './ui-preferences.tsx';

afterEach(cleanup);

function memoryStorage() {
  const values = new Map<string, string>();
  return { values, getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); } };
}

describe('browser display preferences', () => {
  it('restores independent Track and Area choices with a fresh store', () => {
    const storage = memoryStorage();
    const first = createUiPreferences(storage);
    first.setConversation('a:/', 'chat-a');
    first.setConversation('b', 'chat-b');
    first.setAreaExpanded('work', false);
    first.setAreaExpanded('home', true);
    first.setRailCollapsed(true);
    first.setAreaShowsClosed('work', true);
    const restored = createUiPreferences(storage);
    expect(restored.conversation('a:/')).toBe('chat-a');
    expect(restored.conversation('b')).toBe('chat-b');
    expect(restored.conversation('new')).toBeNull();
    expect(restored.areaExpanded('work')).toBe(false);
    expect(restored.areaExpanded('home')).toBe(true);
    expect(restored.railCollapsed()).toBe(true);
    expect(restored.areaShowsClosed('work')).toBe(true);
    expect(restored.areaShowsClosed('home')).toBe(false);
    restored.setConversation('a:/', null);
    const closed = createUiPreferences(storage);
    expect(closed.conversation('a:/')).toBeNull();
    expect(closed.conversation('b')).toBe('chat-b');
  });

  it.each(['{broken', '[]', '{}', '123', '""'])('ignores malformed or unrecognized storage %s', (value) => {
    const preferences = createUiPreferences({ getItem: () => value, setItem: () => {} });
    expect(preferences.conversation('track')).toBeNull();
    expect(preferences.areaExpanded('area')).toBe(true);
    expect(preferences.areaShowsClosed('area')).toBe(false);
    expect(preferences.railCollapsed()).toBeNull();
  });

  it('remembers a preview block\'s device choice per Track and key across instances (#1780)', () => {
    const storage = memoryStorage();
    const first = createUiPreferences(storage);
    first.setPreviewViewport('t1', 'fe', 'mobile');
    first.setPreviewViewport('t2', 'fe', 'desktop');
    const restored = createUiPreferences(storage);
    expect(restored.previewViewport('t1', 'fe')).toBe('mobile');
    expect(restored.previewViewport('t2', 'fe')).toBe('desktop');
    expect(restored.previewViewport('t1', 'api')).toBeNull();
    const throwing = createUiPreferences({
      getItem: () => { throw new Error('denied'); }, setItem: () => { throw new Error('denied'); },
    });
    expect(throwing.previewViewport('t1', 'fe')).toBeNull();
    throwing.setPreviewViewport('t1', 'fe', 'mobile');
    expect(throwing.previewViewport('t1', 'fe')).toBe('mobile');
  });

  /* The shell applies it (`app/shell/drawer-width.browser.test.tsx`); this is what is remembered. */
  describe('the conversation drawer width', () => {
    it('is unset by default and remembered per browser across app instances', () => {
      const storage = memoryStorage();
      const first = createUiPreferences(storage);
      expect(first.drawerWidth()).toBeNull();
      first.setDrawerWidth(47.5);
      expect(createUiPreferences(storage).drawerWidth()).toBe(47.5);
      first.setDrawerWidth(null);
      expect(createUiPreferences(storage).drawerWidth()).toBeNull();
    });

    /* The production scope is `[origin, userId, dbInstanceId]` and `dbInstanceId` changes on every kernel boot. */
    it('survives a bundled server restart, and stays per user', () => {
      const storage = memoryStorage();
      const scope = (user: string, boot: string) => JSON.stringify(['https://server.test', user, boot]);
      const first = createUiPreferences(storage);
      first.setRecoveryScope(scope('owner', 'boot-1'));
      first.setConversation('track', 'chat');
      first.setDrawerWidth(52);

      const restarted = createUiPreferences(storage);
      restarted.setRecoveryScope(scope('owner', 'boot-2'));
      expect(restarted.conversation('track')).toBeNull();
      expect(restarted.drawerWidth()).toBe(52);
      first.setRecoveryScope(scope('another-user', 'boot-2'));
      expect(first.drawerWidth()).toBeNull();
    });

    it('keeps the width in memory when browser storage throws', () => {
      const preferences = createUiPreferences({ getItem() { throw new Error('denied'); }, setItem() { throw new Error('quota'); } });
      expect(preferences.drawerWidth()).toBeNull();
      preferences.setDrawerWidth(30);
      expect(preferences.drawerWidth()).toBe(30);
    });

    it.each(['true', '"wide"', '"-4"', '"0"', '"Infinity"', '42'])('reads %s as no width', (stored) => {
      expect(createUiPreferences({ getItem: () => stored, setItem: () => {} }).drawerWidth()).toBeNull();
    });
  });

  it('rejects valid JSON of the wrong preference type', () => {
    const string = createUiPreferences({ getItem: () => '"chat"', setItem: () => {} });
    expect(string.areaExpanded('area')).toBe(true);
    expect(string.railCollapsed()).toBeNull();
    const boolean = createUiPreferences({ getItem: () => 'false', setItem: () => {} });
    expect(boolean.conversation('track')).toBeNull();
  });

  it('keeps choices in memory when browser storage throws', () => {
    const preferences = createUiPreferences({
      getItem() { throw new Error('denied'); }, setItem() { throw new Error('quota'); },
    });
    preferences.setConversation('track', 'chat');
    preferences.setAreaExpanded('area', false);
    preferences.setRailCollapsed(false);
    expect(preferences.conversation('track')).toBe('chat');
    expect(preferences.areaExpanded('area')).toBe(false);
    expect(preferences.railCollapsed()).toBe(false);
    preferences.setConversation('track', null);
    expect(preferences.conversation('track')).toBeNull();
  });

  it('does not overwrite choices for other entities from a second app instance', () => {
    const storage = memoryStorage();
    const first = createUiPreferences(storage);
    const second = createUiPreferences(storage);
    first.setConversation('a', 'chat-a');
    second.setConversation('b', 'chat-b');
    first.setAreaExpanded('area-a', false);
    second.setAreaExpanded('area-b', false);
    const restored = createUiPreferences(storage);
    expect(restored.conversation('a')).toBe('chat-a');
    expect(restored.conversation('b')).toBe('chat-b');
    expect(restored.areaExpanded('area-a')).toBe(false);
    expect(restored.areaExpanded('area-b')).toBe(false);
  });
});

it('seeds the database scope from the stable database id, never the per-boot instance id', () => {
  const storage = memoryStorage();
  storage.values.set(DB_INSTANCE_ID_KEY, 'boot-42');
  expect(createUiPreferences(storage).databaseScope()).toBeNull();
  storage.values.set(DATABASE_ID_KEY, 'db1');
  expect(createUiPreferences(storage).databaseScope()).toBe('db1');
});

it('isolates remembered conversation selection by verified recovery scope', () => {
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
  const preferences = createUiPreferences(storage);
  preferences.setRecoveryScope('origin/owner/db-a'); preferences.setConversation('track', 'conversation-a');
  preferences.setRecoveryScope('origin/owner/db-b'); expect(preferences.conversation('track')).toBeNull();
  preferences.setConversation('track', 'conversation-b');
  const resumed = createUiPreferences(storage); resumed.setRecoveryScope('origin/owner/db-b');
  expect(resumed.conversation('track')).toBe('conversation-b');
});

it('keeps sidebar display choices across server boots and scopes them to the user', () => {
  const storage = memoryStorage();
  const scope = (user: string, boot: string) => JSON.stringify(['https://server.test', user, boot]);
  const first = createUiPreferences(storage);
  first.setRecoveryScope(scope('owner', 'boot-1'));
  first.setSidebarGroupVisible('unread', true);
  first.setSidebarGroupVisible('running', true);
  first.setSidebarGroupExpanded('pinned', false);
  const restored = createUiPreferences(storage);
  restored.setRecoveryScope(scope('owner', 'boot-2'));
  expect(restored.sidebarGroupVisible('unread')).toBe(true);
  expect(restored.sidebarGroupVisible('running')).toBe(true);
  expect(restored.sidebarGroupExpanded('pinned')).toBe(false);
  restored.setRecoveryScope(scope('other', 'boot-2'));
  expect(restored.sidebarGroupVisible('unread')).toBe(false);
  expect(restored.sidebarGroupVisible('running')).toBe(false);
  expect(restored.sidebarGroupExpanded('pinned')).toBe(true);
});


it('keeps order and visibility per user across boots, with an in-memory fallback', () => {
  const storage = memoryStorage();
  const scope = (user: string, boot: string) => JSON.stringify(['https://server.test', user, boot]);
  const first = createUiPreferences(storage);
  first.setRecoveryScope(scope('owner', 'one'));
  first.setSidebarOrder('sections', ['areas', 'pinned', 'waiting']);
  first.setSidebarOrder('areas', ['b', 'a']);
  first.setSidebarGroupVisible('waiting', false);
  first.setSidebarGroupVisible('area:a', false);
  const restored = createUiPreferences(storage);
  restored.setRecoveryScope(scope('owner', 'two'));
  expect(restored.sidebarOrder('sections', ['waiting', 'pinned', 'areas'])).toEqual(['areas', 'pinned', 'waiting']);
  expect(restored.sidebarOrder('areas', ['a', 'b', 'new'])).toEqual(['b', 'a', 'new']);
  expect(restored.sidebarGroupVisible('waiting')).toBe(false);
  expect(restored.sidebarGroupVisible('area:a')).toBe(false);
  restored.setRecoveryScope(scope('other', 'two'));
  expect(restored.sidebarOrder('areas', ['a', 'b'])).toEqual(['a', 'b']);
  expect(restored.sidebarGroupVisible('waiting')).toBe(true);
  expect(restored.sidebarGroupVisible('area:a')).toBe(true);
  const memory = createUiPreferences({ getItem() { throw new Error('denied'); }, setItem() { throw new Error('quota'); } });
  memory.setSidebarOrder('areas', ['b', 'a']);
  memory.setSidebarGroupVisible('area:a', false);
  expect(memory.sidebarOrder('areas', ['a', 'b'])).toEqual(['b', 'a']);
  expect(memory.sidebarGroupVisible('area:a')).toBe(false);
});


it('isolates Area pins by database and user while surviving restart', () => {
  const storage = memoryStorage();
  const preferences = createUiPreferences(storage);
  preferences.setRecoveryScope(JSON.stringify(['origin', 'owner', 'boot1']));
  preferences.setDatabaseScope('db1');
  preferences.setAreaTrackPinned('area', 't', true);
  expect(preferences.areaTrackPinned('other-area', 't')).toBe(false);
  preferences.setDatabaseScope('db2');
  expect(preferences.areaTrackPinned('area', 't')).toBe(false);
  preferences.setDatabaseScope('db1');
  preferences.setRecoveryScope(JSON.stringify(['origin', 'owner', 'boot2']));
  expect(preferences.areaTrackPinned('area', 't')).toBe(true);
  preferences.setRecoveryScope(JSON.stringify(['origin', 'guest', 'boot2']));
  expect(preferences.areaTrackPinned('area', 't')).toBe(false);
  preferences.setDatabaseScope(null);
  preferences.setAreaTrackPinned('area', 't', true);
  expect(preferences.areaTrackPinned('area', 't')).toBe(false);
});

it('remembers guide enrollment and steps per stable database and revision', () => {
  const storage = memoryStorage();
  const first = createUiPreferences(storage);
  first.setGuideEnabled(true); // Unconfirmed identity writes nothing.
  expect(storage.values.size).toBe(0);
  first.setDatabaseScope('first'); first.setGuideEnabled(true); first.setGuideStepSeen(1, 'options', true);
  const restored = createUiPreferences(storage); restored.setDatabaseScope('first');
  expect(restored.guideEnabled()).toBe(true);
  expect(restored.guideStepSeen(1, 'options')).toBe(true);
  expect(restored.guideStepSeen(2, 'options')).toBe(false);
  restored.setDatabaseScope('second');
  expect(restored.guideEnabled()).toBe(null);
  expect(restored.guideStepSeen(1, 'options')).toBe(false);
  restored.setDatabaseScope('first'); restored.setGuideEnabled(false);
  expect(first.guideEnabled()).toBe(true); // Independent stores refresh on the next boot.
  const skipped = createUiPreferences(storage); skipped.setDatabaseScope('first');
  expect(skipped.guideEnabled()).toBe(false);
});

it('keeps guide browser choices across recovery boots and separates user namespaces', () => {
  const preferences = createUiPreferences(memoryStorage());
  preferences.setDatabaseScope('db');
  preferences.setRecoveryScope(JSON.stringify(['https://example.test', 'one', 'boot-a']));
  preferences.setGuideEnabled(true); preferences.setGuideStepSeen(1, 'options', true);
  preferences.setRecoveryScope(JSON.stringify(['https://example.test', 'one', 'boot-b']));
  expect(preferences.guideStepSeen(1, 'options')).toBe(true);
  preferences.setRecoveryScope(JSON.stringify(['https://example.test', 'two', 'boot-b']));
  expect(preferences.guideEnabled()).toBe(null);
  expect(preferences.guideStepSeen(1, 'options')).toBe(false);
});

it('accepts a guide commit only for the live scope and publishes its fields together', () => {
  const preferences = createUiPreferences(); preferences.setDatabaseScope('first');
  const first = preferences.guideScope()!;
  preferences.setDatabaseScope(null);
  expect(preferences.commitGuideProgress(first, { version: 1, reset: [], seen: ['options'], enabled: true })).toBe(false);
  preferences.setDatabaseScope('second');
  expect(preferences.commitGuideProgress(first, { version: 1, reset: [], seen: ['options'], enabled: true })).toBe(false);
  expect(preferences.guideStepSeen(1, 'options')).toBe(false);
  const second = preferences.guideScope()!;
  const observed: Array<readonly [boolean | null, boolean]> = [];
  preferences.subscribe(() => observed.push([preferences.guideEnabled(), preferences.guideStepSeen(1, 'options')]));
  expect(preferences.commitGuideProgress(second, { version: 1, reset: ['options'], seen: ['options'], enabled: true })).toBe(true);
  expect(observed).toEqual([[true, true]]);
  preferences.setRecoveryScope(JSON.stringify(['https://example.test', 'other-user', 'boot']));
  expect(preferences.commitGuideProgress(second, { version: 1, reset: [], seen: ['options'], enabled: true })).toBe(false);
});
