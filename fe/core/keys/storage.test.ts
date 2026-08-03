import { describe, expect, it } from 'vitest';

import { DB_INSTANCE_ID_KEY, SYNC_CURSOR_KEY, THEME_KEY, createStorageKey } from './storage.js';

describe('stable storage keys', () => {
  it('preserves deployed key spellings independently of production constants', () => {
    expect(SYNC_CURSOR_KEY).toBe('calm:sync:cursor');
    expect(DB_INSTANCE_ID_KEY).toBe('calm:db_instance_id');
    expect(THEME_KEY).toBe('calm.theme');
  });

  it('creates namespaced colon-separated keys', () => {
    expect(createStorageKey('draft', 'report', '42')).toBe('calm:draft:report:42');
  });

  it('rejects ambiguous dynamic key segments', () => {
    expect(() => createStorageKey('draft', '')).toThrow(TypeError);
    expect(() => createStorageKey('draft:report')).toThrow(TypeError);
  });
});
