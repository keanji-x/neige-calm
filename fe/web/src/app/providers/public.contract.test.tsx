import { describe, expect, it } from 'vitest';
import { DB_INSTANCE_ID_KEY, SYNC_CURSOR_KEY } from '../../../../core/keys/storage.ts';
import { PERSIST_OPTIONS, WEB_COMPAT_VERSION, queryClient, retryUnless401 } from './public.tsx';

describe('app/providers contracts', () => {
  it('INV-APP-004 exports stable module singletons', async () => {
    const again = await import('./public.tsx');
    expect(again.queryClient).toBe(queryClient);
    expect(again.PERSIST_OPTIONS).toBe(PERSIST_OPTIONS);
    expect(Object.isFrozen(PERSIST_OPTIONS)).toBe(true);
  });

  it('INV-APP-059 INV-APP-060 never retries 401 and retries other failures once', () => {
    expect(retryUnless401(0, { kind: 'unauthorized', status: 401 })).toBe(false);
    expect(retryUnless401(0, { kind: 'http', status: 500 })).toBe(true);
    expect(retryUnless401(1, new Error('network'))).toBe(false);
  });

  it('INV-APP-016 uses canonical storage and persistence identifiers', () => {
    expect(PERSIST_OPTIONS.idbDatabaseName).toBe('neige-calm');
    expect(PERSIST_OPTIONS.dbInstanceKey).toBe(DB_INSTANCE_ID_KEY);
    expect(PERSIST_OPTIONS.syncCursorKey).toBe(SYNC_CURSOR_KEY);
    expect(WEB_COMPAT_VERSION).toBeGreaterThan(0);
  });
});
