import { expect, it } from 'vitest';
import { decodeRecoveryContext, recoveryPage } from './context.js';
it('persists only allowlisted resource IDs and page layout without arbitrary URL data', () => {
  expect(recoveryPage('/next/track/t1', '?file=secrets.txt&token=secret&card=c1&panel=cards')).toEqual({
    route: '/next/track/t1?card=c1&panel=cards', kind: 'track', pane: 'cards',
  });
  expect(recoveryPage('/next/area/a1/new')).toEqual({ route: '/next/', kind: 'today', pane: null });
  expect(recoveryPage('/next/track/%2e%2e').kind).toBe('today');
});
it('corrupt, future, cross-origin and unbounded presentation records fail closed', () => {
  const scope = { origin: 'https://example.test', userId: 'owner', dbInstanceId: 'db' };
  const record = { schemaVersion: 1, scope, page: recoveryPage('/next/track/t1'), scroll: [{ region: 'page', top: 50, left: 0 }] };
  expect(decodeRecoveryContext(JSON.stringify(record), scope.origin)).toEqual(record);
  for (const invalid of [ { ...record, schemaVersion: 2 }, { ...record, scroll: [{ region: 'page', top: -1, left: 0 }] },
    { ...record, page: { ...record.page, route: 'https://other.test' } }, { ...record, scope: { ...scope, origin: 'https://other.test' } } ]) {
    expect(decodeRecoveryContext(JSON.stringify(invalid), scope.origin)).toBeNull();
  }
});
