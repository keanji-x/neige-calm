import { describe, expect, it } from 'vitest';

import {
  coveFolderWireSchema, coveFoldersOperation, coveListOperation, coveOf, coveWireSchema,
  sortedCoveFolders, sortedCoves, toCove, toCoveFolder, visibleCoves,
  type Cove, type CoveFolder,
} from './cove.js';

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

function folder(overrides: Partial<CoveFolder>): CoveFolder {
  return { id: 1, coveId: 'c1', path: '/srv/a', repoIdentity: null, repoIdentityProbedAt: null, createdAt: 0, ...overrides };
}

describe('cove folders', () => {
  it('decodes a folder row and defaults an unprobed repo identity to null', () => {
    const wire = coveFolderWireSchema.parse({ id: 7, cove_id: 'c1', path: '/srv/a', created_at: 5 });
    expect(toCoveFolder(wire)).toEqual({
      id: 7, coveId: 'c1', path: '/srv/a', repoIdentity: null, repoIdentityProbedAt: null, createdAt: 5,
    });
  });

  it('keeps a probed repo identity rather than flattening it', () => {
    const wire = coveFolderWireSchema.parse({
      id: 7, cove_id: 'c1', path: '/srv/a', repo_identity: 'you/repo', repo_identity_probed_at: 9, created_at: 5,
    });
    expect(toCoveFolder(wire)).toMatchObject({ repoIdentity: 'you/repo', repoIdentityProbedAt: 9 });
  });

  it('reads the folders of one cove, id-encoded into the path', () => {
    expect(coveFoldersOperation('c/1')).toMatchObject({ method: 'GET', path: '/api/coves/c%2F1/folders' });
  });

  /* INV-NEWWAVE-003 — the new-wave form takes `[0]` as the folder a wave runs
     in when it shows no choice, so "first" has to be one deterministic thing. */
  it('orders by path and breaks ties by id without mutating the input', () => {
    const list = [folder({ id: 3, path: '/srv/b' }), folder({ id: 2, path: '/srv/a' }), folder({ id: 1, path: '/srv/a' })];
    expect(sortedCoveFolders(list).map((f) => f.id)).toEqual([1, 2, 3]);
    expect(list.map((f) => f.id)).toEqual([3, 2, 1]);
  });
});
