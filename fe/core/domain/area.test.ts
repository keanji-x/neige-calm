import { describe, expect, it } from 'vitest';

import {
  asFolderConflict, areaFolderWireSchema, areaFoldersOperation, areaListOperation, areaOf,
  areaWireSchema, folderConflictMessage, sortedAreaFolders, sortedAreas, toArea, toAreaFolder,
  visibleAreas, type Area, type AreaFolder,
} from './area.js';

const baseWire = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, created_at: 1, updated_at: 1 };

function area(overrides: Partial<Area>): Area {
  return {
    id: 'c', name: 'n', color: '#000', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0, ...overrides,
  };
}

describe('area wire decode', () => {
  it('defaults a pre-#175 payload without kind to user', () => {
    expect(areaWireSchema.parse(baseWire)).toMatchObject({
      kind: 'user', default_template_id: null, default_cwd: null,
    });
  });

  it('keeps an explicit system kind', () => {
    expect(areaWireSchema.parse({ ...baseWire, kind: 'system' }).kind).toBe('system');
  });

  it('rejects an unknown kind', () => {
    expect(areaWireSchema.safeParse({ ...baseWire, kind: 'kernel' }).success).toBe(false);
  });

  it('maps the wire row onto the camelCase domain shape', () => {
    expect(toArea(areaWireSchema.parse(baseWire)))
      .toEqual(area({ id: 'c1', name: 'Work', color: '#5B8DEF', createdAt: 1, updatedAt: 1 }));
  });

  it('preserves both Area defaults as required nullable domain fields', () => {
    const parsed = areaWireSchema.parse({
      ...baseWire, default_template_id: 'small-change', default_cwd: '/srv/work',
    });
    expect(toArea(parsed)).toMatchObject({
      defaultTemplateId: 'small-change', defaultCwd: '/srv/work',
    });
  });

  it('reads the areas list from the documented path', () => {
    expect(areaListOperation()).toMatchObject({ method: 'GET', path: '/api/areas' });
  });
});

describe('visibleAreas', () => {
  // E2E-INV-SHELL-003: this is the client-side half of the #175 filter.
  it('drops the kernel system area and keeps user areas', () => {
    const system = area({ id: 'sys', kind: 'system' });
    const user = area({ id: 'u', kind: 'user' });
    expect(visibleAreas([system, user]).map((c) => c.id)).toEqual(['u']);
  });

  it('returns an empty list for a workspace that only has the system area', () => {
    expect(visibleAreas([area({ id: 'sys', kind: 'system' })])).toEqual([]);
  });
});

describe('area ordering and lookup', () => {
  it('orders by sort and breaks ties by id without mutating the input', () => {
    const list = [area({ id: 'b', sort: 2 }), area({ id: 'a', sort: 2 }), area({ id: 'z', sort: 1 })];
    expect(sortedAreas(list).map((c) => c.id)).toEqual(['z', 'a', 'b']);
    expect(list.map((c) => c.id)).toEqual(['b', 'a', 'z']);
  });

  it('resolves an area by id and reports a miss as undefined', () => {
    const list = [area({ id: 'a' })];
    expect(areaOf('a', list)?.id).toBe('a');
    expect(areaOf('nope', list)).toBeUndefined();
  });
});

function folder(overrides: Partial<AreaFolder>): AreaFolder {
  return { id: 1, areaId: 'c1', path: '/srv/a', repoIdentity: null, repoIdentityProbedAt: null, createdAt: 0, ...overrides };
}

describe('area folders', () => {
  it('decodes a folder row and defaults an unprobed repo identity to null', () => {
    const wire = areaFolderWireSchema.parse({ id: 7, area_id: 'c1', path: '/srv/a', created_at: 5 });
    expect(toAreaFolder(wire)).toEqual({
      id: 7, areaId: 'c1', path: '/srv/a', repoIdentity: null, repoIdentityProbedAt: null, createdAt: 5,
    });
  });

  it('keeps a probed repo identity rather than flattening it', () => {
    const wire = areaFolderWireSchema.parse({
      id: 7, area_id: 'c1', path: '/srv/a', repo_identity: 'you/repo', repo_identity_probed_at: 9, created_at: 5,
    });
    expect(toAreaFolder(wire)).toMatchObject({ repoIdentity: 'you/repo', repoIdentityProbedAt: 9 });
  });

  it('reads the folders of one area, id-encoded into the path', () => {
    expect(areaFoldersOperation('c/1')).toMatchObject({ method: 'GET', path: '/api/areas/c%2F1/folders' });
  });

  /* Path ascending, ties broken by id — insertion order is not a display order. */
  it('orders by path and breaks ties by id without mutating the input', () => {
    const list = [folder({ id: 3, path: '/srv/b' }), folder({ id: 2, path: '/srv/a' }), folder({ id: 1, path: '/srv/a' })];
    expect(sortedAreaFolders(list).map((f) => f.id)).toEqual([1, 2, 3]);
    expect(list.map((f) => f.id)).toEqual([3, 2, 1]);
  });
});

/*
 * #1147 S3 — `POST /api/tracks` answers a folder clash with a structured body
 * that has no `error` key, so `core/api/client.ts` normalises it to the bare
 * status text ("Conflict"). These are what turns that into a sentence.
 */
describe('folder conflict decode', () => {
  const conflict = {
    folder_id: 4, area_id: 'c2', conflict_path: '/srv/app', conflict_kind: 'descendant',
  } as const;

  it('decodes the kernel 409 body', () => {
    expect(asFolderConflict(conflict)).toEqual(conflict);
  });

  it('refuses any other error body rather than guessing at one', () => {
    expect(asFolderConflict({ error: 'Conflict' })).toBeNull();
    expect(asFolderConflict(null)).toBeNull();
    expect(asFolderConflict('Conflict')).toBeNull();
    // An unknown kind is not a fourth message; it is a body we cannot read.
    expect(asFolderConflict({ ...conflict, conflict_kind: 'sibling' })).toBeNull();
    expect(asFolderConflict({ ...conflict, area_id: 7 })).toBeNull();
  });

  it('names the owning area, the path, and a different remedy per kind', () => {
    const descendant = folderConflictMessage(conflict, 'Atlas');
    expect(descendant).toContain('area “Atlas”');
    expect(descendant).toContain('/srv/app');
    expect(descendant).toContain('pick a different folder');

    const ancestor = folderConflictMessage({ ...conflict, conflict_kind: 'ancestor' }, 'Atlas');
    expect(ancestor).toContain('narrower claim');
    expect(ancestor).not.toBe(descendant);

    const equal = folderConflictMessage({ ...conflict, conflict_kind: 'equal' }, 'Atlas');
    expect(equal).toContain('That exact folder');
    expect(equal).not.toBe(descendant);
    expect(equal).not.toBe(ancestor);
  });

  /* The area may have been made in another tab, or deleted between the
     conflict and this render. A uuid on screen would be worse than a phrase. */
  it('degrades to "another area" rather than printing an id', () => {
    const message = folderConflictMessage(conflict, null);
    expect(message).toContain('another area');
    expect(message).not.toContain('c2');
  });
});
