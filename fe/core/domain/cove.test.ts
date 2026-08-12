import { describe, expect, it } from 'vitest';

import { coveListOperation, coveOf, coveWireSchema, sortedCoves, toCove, visibleCoves, type Cove } from './cove.js';

const baseWire = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, created_at: 1, updated_at: 1 };

function cove(overrides: Partial<Cove>): Cove {
  return { id: 'c', name: 'n', color: '#000', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

describe('cove wire decode', () => {
  it('defaults a pre-#175 payload without kind to user', () => {
    expect(coveWireSchema.parse(baseWire).kind).toBe('user');
  });

  it('keeps an explicit system kind', () => {
    expect(coveWireSchema.parse({ ...baseWire, kind: 'system' }).kind).toBe('system');
  });

  it('rejects an unknown kind', () => {
    expect(coveWireSchema.safeParse({ ...baseWire, kind: 'kernel' }).success).toBe(false);
  });

  it('maps the wire row onto the camelCase domain shape', () => {
    expect(toCove(coveWireSchema.parse(baseWire)))
      .toEqual(cove({ id: 'c1', name: 'Work', color: '#5B8DEF', createdAt: 1, updatedAt: 1 }));
  });

  it('reads the coves list from the documented path', () => {
    expect(coveListOperation()).toMatchObject({ method: 'GET', path: '/api/coves' });
  });
});

describe('visibleCoves', () => {
  // E2E-INV-SHELL-003: this is the client-side half of the #175 filter.
  it('drops the kernel system cove and keeps user coves', () => {
    const system = cove({ id: 'sys', kind: 'system' });
    const user = cove({ id: 'u', kind: 'user' });
    expect(visibleCoves([system, user]).map((c) => c.id)).toEqual(['u']);
  });

  it('returns an empty list for a workspace that only has the system cove', () => {
    expect(visibleCoves([cove({ id: 'sys', kind: 'system' })])).toEqual([]);
  });
});

describe('cove ordering and lookup', () => {
  it('orders by sort and breaks ties by id without mutating the input', () => {
    const list = [cove({ id: 'b', sort: 2 }), cove({ id: 'a', sort: 2 }), cove({ id: 'z', sort: 1 })];
    expect(sortedCoves(list).map((c) => c.id)).toEqual(['z', 'a', 'b']);
    expect(list.map((c) => c.id)).toEqual(['b', 'a', 'z']);
  });

  it('resolves a cove by id and reports a miss as undefined', () => {
    const list = [cove({ id: 'a' })];
    expect(coveOf('a', list)?.id).toBe('a');
    expect(coveOf('nope', list)).toBeUndefined();
  });
});
