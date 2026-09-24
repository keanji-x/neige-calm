import { describe, expect, it } from 'vitest';

import { DATABASE_ID_KEY, DB_INSTANCE_ID_KEY } from '../../../../core/keys/storage.ts';
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
    // The scope is entered with a server time *before* the activity, so the baseline does not hide it.
    preferences.setReadScope('db-a', 50);
    expect(preferences.isUnread('track', 'previously-visible', 100)).toBe(true);
    preferences.markRead('track', 'currently-visible', 100);
    expect(preferences.isUnread('track', 'currently-visible', 100)).toBe(false);
    preferences.setReadScope('db-b', 50);
    expect(preferences.isUnread('track', 'currently-visible', 100)).toBe(true);
  });

  it('keeps read receipts across reloads, accepts only newer acknowledgements, and isolates databases', () => {
    const storage = memoryStorage();
    storage.values.set(DATABASE_ID_KEY, 'db-a');
    const preferences = createUiPreferences(storage);
    expect(preferences.isUnread('track', 'a', 100)).toBe(true);
    preferences.markRead('track', 'a', 100);
    preferences.markRead('track', 'a', 90);
    expect(preferences.isUnread('track', 'a', 100)).toBe(false);
    expect(preferences.isUnread('track', 'a', 101)).toBe(true);
    const restored = createUiPreferences(storage);
    expect(restored.isUnread('track', 'a', 100)).toBe(false);
    expect(restored.isUnread('conversation', 'a', 100)).toBe(true);
    storage.values.set(DATABASE_ID_KEY, 'db-b');
    restored.setReadScope('db-b', 50);
    expect(restored.isUnread('track', 'a', 100)).toBe(true);
  });

  it('first_scope_entry_marks_everything_read', () => {
    const now = 1_000_000;
    const storage = memoryStorage();
    const preferences = createUiPreferences(storage);
    // A null scope first (the compat verdict is still pending): there is no database to key a baseline on yet.
    preferences.setReadScope(null, now);
    expect(storage.values.size).toBe(0);
    preferences.setReadScope('db1', now);
    // Everything that completed before the first look reads as read…
    expect(preferences.isUnread('track', 't', now - 1)).toBe(false);
    expect(preferences.isUnread('track', 't', now)).toBe(false);
    expect(preferences.isUnread('conversation', 'c', now - 1)).toBe(false);
    // …and what completes after it is unread until acknowledged.
    expect(preferences.isUnread('track', 't', now + 1)).toBe(true);
    // Written once, on disk, so a reload keeps the baseline without a receipt for every track.
    expect([...storage.values.keys()].filter((key) => key.includes('baseline'))).toHaveLength(1);
    const restored = createUiPreferences(storage);
    restored.setReadScope('db1', now + 500);
    expect(restored.isUnread('track', 'never-visited', now - 1)).toBe(false);
    expect(restored.isUnread('track', 'never-visited', now + 1)).toBe(true);
  });

  it('construction_seeds_from_database_id_key', () => {
    const storage = memoryStorage();
    storage.values.set(DB_INSTANCE_ID_KEY, 'boot-42');
    // Only the per-boot instance id is present: it must not become a scope.
    expect(createUiPreferences(storage).readScope()).toBeNull();
    storage.values.set(DATABASE_ID_KEY, 'db1');
    expect(createUiPreferences(storage).readScope()).toBe('db1');
  });

  it('null_scope_is_never_unread', () => {
    const now = 1_000_000;
    const storage = memoryStorage();
    storage.values.set(DATABASE_ID_KEY, 'db1');
    const preferences = createUiPreferences(storage);
    expect(preferences.isUnread('track', 't', now)).toBe(true);
    // Under a null scope nothing is unread rather than everything.
    preferences.setReadScope(null);
    expect(preferences.isUnread('track', 't', now)).toBe(false);
    expect(preferences.isUnread('conversation', 'c', now)).toBe(false);
    preferences.setReadScope('db1', now);
    expect(preferences.isUnread('track', 't', now + 1)).toBe(true);
    expect(preferences.isUnread('track', 't', now)).toBe(false);
  });
});

it('does not overwrite a newer acknowledgement from another tab', () => {
  const storage = memoryStorage();
  storage.values.set(DATABASE_ID_KEY, 'same-db');
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
    preferences.setRecoveryScope('origin/owner/db-a'); preferences.setReadScope('db-a', 5);
  }
  first.markRead('conversation', 'visible', 20);
  second.markRead('conversation', 'visible', 10);
  const restored = createUiPreferences(storage);
  restored.setReadScope('db-a', 5); restored.setRecoveryScope('origin/owner/db-a');
  expect(restored.isUnread('conversation', 'visible', 20)).toBe(false);
  expect(restored.isUnread('conversation', 'visible', 21)).toBe(true);
  restored.setRecoveryScope('origin/another-owner/db-a');
  expect(restored.isUnread('conversation', 'visible', 20)).toBe(true);
});

// The recovery scope carries `dbInstanceId`, which changes on every kernel boot; receipts and
// the baseline live beside it, not under it, or a restart would re-stamp the baseline and swallow completions.
it('receipts_survive_a_recovery_scope_change_on_the_same_database', () => {
  const storage = memoryStorage();
  const writes: string[] = [];
  const recorded = { getItem: storage.getItem, setItem: (key: string, value: string) => { writes.push(key); storage.setItem(key, value); } };
  const bootOne = JSON.stringify(['https://server.test', 'owner', 'boot-1']);
  const bootTwo = JSON.stringify(['https://server.test', 'owner', 'boot-2']);
  const preferences = createUiPreferences(recorded);
  preferences.setRecoveryScope(bootOne);
  preferences.setReadScope('db', 100);
  const baselineKeys = () => [...storage.values.keys()].filter((key) => key.includes('baseline'));
  expect(baselineKeys()).toHaveLength(1);
  expect(baselineKeys()[0]).not.toContain('boot-1');
  expect(preferences.isUnread('track', 't', 150)).toBe(true);
  // The kernel restarts: a new boot id, the same database at a later server time.
  preferences.setRecoveryScope(bootTwo);
  preferences.setReadScope('db', 200);
  expect(preferences.isUnread('track', 't', 150)).toBe(true);
  expect(baselineKeys()).toHaveLength(1);
  expect(storage.getItem(baselineKeys()[0])).toBe(JSON.stringify('100'));
  expect(writes.filter((key) => key.includes('baseline'))).toHaveLength(1);
  // A receipt taken under one boot is read back under the next, also by a fresh app instance.
  preferences.markRead('track', 't', 150);
  const restored = createUiPreferences(recorded);
  restored.setRecoveryScope(bootTwo);
  restored.setReadScope('db', 300);
  expect(restored.isUnread('track', 't', 150)).toBe(false);
  expect(restored.isUnread('track', 't', 151)).toBe(true);
  expect(restored.isUnread('track', 'never-visited', 120)).toBe(true);
  expect(baselineKeys()).toHaveLength(1);
});

it('receipts_are_isolated_per_user', () => {
  const storage = memoryStorage();
  const owner = createUiPreferences(storage);
  owner.setRecoveryScope(JSON.stringify(['https://server.test', 'owner', 'boot-1']));
  owner.setReadScope('db', 100);
  owner.markRead('track', 't', 150);
  expect(owner.isUnread('track', 't', 150)).toBe(false);
  // Same origin, same boot, same database — another user sees neither the
  // owner's receipt nor the owner's baseline.
  const guest = createUiPreferences(storage);
  guest.setRecoveryScope(JSON.stringify(['https://server.test', 'guest', 'boot-1']));
  guest.setReadScope('db', 100);
  expect(guest.isUnread('track', 't', 150)).toBe(true);
  expect([...storage.values.keys()].filter((key) => key.includes('baseline'))).toHaveLength(2);
  // Display preferences keep the per-boot recovery scope untouched.
  owner.setConversation('track', 'conversation-a');
  const ownerAgain = createUiPreferences(storage);
  ownerAgain.setRecoveryScope(JSON.stringify(['https://server.test', 'owner', 'boot-2']));
  expect(ownerAgain.conversation('track')).toBeNull();
});
