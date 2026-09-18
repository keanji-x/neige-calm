import { describe, expect, it } from 'vitest';

import { DB_INSTANCE_ID_KEY } from '../../../../core/keys/storage.ts';
import { createUiPreferences } from './ui-preferences.tsx';

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
    const restored = createUiPreferences(storage);
    expect(restored.conversation('a:/')).toBe('chat-a');
    expect(restored.conversation('b')).toBe('chat-b');
    expect(restored.conversation('new')).toBeNull();
    expect(restored.areaExpanded('work')).toBe(false);
    expect(restored.areaExpanded('home')).toBe(true);
    expect(restored.railCollapsed()).toBe(true);
    restored.setConversation('a:/', null);
    const closed = createUiPreferences(storage);
    expect(closed.conversation('a:/')).toBeNull();
    expect(closed.conversation('b')).toBe('chat-b');
  });

  it.each(['{broken', '[]', '{}', '123', '""'])('ignores malformed or unrecognized storage %s', (value) => {
    const preferences = createUiPreferences({ getItem: () => value, setItem: () => {} });
    expect(preferences.conversation('track')).toBeNull();
    expect(preferences.areaExpanded('area')).toBe(true);
    expect(preferences.railCollapsed()).toBeNull();
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

describe('local read receipts', () => {
  it('keeps unscoped receipts in memory without transferring them into a discovered database', () => {
    const storage = memoryStorage();
    const preferences = createUiPreferences(storage);
    preferences.markRead('track', 'previously-visible', 100);
    expect(preferences.isUnread('track', 'previously-visible', 100)).toBe(false);
    expect(storage.values.size).toBe(0);
    preferences.setReadScope('db-a');
    expect(preferences.isUnread('track', 'previously-visible', 100)).toBe(true);
    preferences.markRead('track', 'currently-visible', 100);
    expect(preferences.isUnread('track', 'currently-visible', 100)).toBe(false);
    preferences.setReadScope('db-b');
    expect(preferences.isUnread('track', 'currently-visible', 100)).toBe(true);
  });

  it('keeps read receipts across reloads, accepts only newer acknowledgements, and isolates databases', () => {
    const storage = memoryStorage();
    storage.values.set(DB_INSTANCE_ID_KEY, 'db-a');
    const preferences = createUiPreferences(storage);
    expect(preferences.isUnread('track', 'a', 100)).toBe(true);
    preferences.markRead('track', 'a', 100);
    preferences.markRead('track', 'a', 90);
    expect(preferences.isUnread('track', 'a', 100)).toBe(false);
    expect(preferences.isUnread('track', 'a', 101)).toBe(true);
    const restored = createUiPreferences(storage);
    expect(restored.isUnread('track', 'a', 100)).toBe(false);
    expect(restored.isUnread('conversation', 'a', 100)).toBe(true);
    storage.values.set(DB_INSTANCE_ID_KEY, 'db-b');
    restored.setReadScope('db-b');
    expect(restored.isUnread('track', 'a', 100)).toBe(true);
  });
});

it('does not overwrite a newer acknowledgement from another tab', () => {
  const storage = memoryStorage();
  storage.values.set(DB_INSTANCE_ID_KEY, 'same-db');
  const first = createUiPreferences(storage);
  const second = createUiPreferences(storage);
  first.markRead('conversation', 'a', 10);
  expect(second.isUnread('conversation', 'a', 15)).toBe(true);
  first.markRead('conversation', 'a', 20);
  second.markRead('conversation', 'a', 15);
  expect(createUiPreferences(storage).isUnread('conversation', 'a', 20)).toBe(false);
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

it('preserves newer cross-tab read receipts within the same recovery scope only', () => {
  const storage = memoryStorage();
  const first = createUiPreferences(storage);
  const second = createUiPreferences(storage);
  for (const preferences of [first, second]) {
    preferences.setRecoveryScope('origin/owner/db-a'); preferences.setReadScope('db-a');
  }
  first.markRead('conversation', 'visible', 20);
  second.markRead('conversation', 'visible', 10);
  const restored = createUiPreferences(storage);
  restored.setReadScope('db-a'); restored.setRecoveryScope('origin/owner/db-a');
  expect(restored.isUnread('conversation', 'visible', 20)).toBe(false);
  expect(restored.isUnread('conversation', 'visible', 21)).toBe(true);
  restored.setRecoveryScope('origin/another-owner/db-a');
  expect(restored.isUnread('conversation', 'visible', 20)).toBe(true);
});
