import { describe, expect, it } from 'vitest';

import {
  directoryListingWireSchema, listDirectoryOperation, readTrackWorkspaceFileOperation,
  toDirectoryListing, trackWorkspaceRawFileUrl,
} from './fs.js';

const WIRE = Object.freeze({
  path: '/home/kenji',
  parent: '/home',
  entries: [
    { name: 'code', is_dir: true },
    { name: 'notes.md', is_dir: false },
  ],
});

/* The production `joinPath` is `ui/directory-browser`'s `joinDirectoryPath`,
   which `core` may not import (`core-no-web-layers`). This stand-in is
   deliberately *not* a re-implementation of it: it records its arguments in a
   shape no real join would produce, so a mapping that ignored the injected
   function — or fed it the wrong parent — cannot pass. The real function is
   driven end to end by `web/src/app/providers/directory.test.ts`. */
const spyJoin = (parent: string, name: string) => `join(${parent}|${name})`;

describe('the listdir wire decodes into the shape the directory browser renders', () => {
  it('maps is_dir to isDirectory and places every entry with the injected join', () => {
    expect(toDirectoryListing(WIRE, spyJoin)).toEqual({
      path: '/home/kenji',
      parent: '/home',
      entries: [
        { name: 'code', path: 'join(/home/kenji|code)', isDirectory: true },
        { name: 'notes.md', path: 'join(/home/kenji|notes.md)', isDirectory: false },
      ],
    });
  });

  it('keeps a null parent null — that is the filesystem root, not a missing value', () => {
    const listing = toDirectoryListing({ path: '/', parent: null, entries: [] }, spyJoin);
    expect(listing.parent).toBeNull();
    expect(listing.entries).toEqual([]);
  });

  it('decodes the kernel body and rejects one that renamed is_dir', () => {
    expect(directoryListingWireSchema.safeParse(WIRE).success).toBe(true);
    expect(directoryListingWireSchema.safeParse({
      path: '/home', parent: null, entries: [{ name: 'code', isDirectory: true }],
    }).success).toBe(false);
  });
});

describe('the listdir operation', () => {
  it('omits the query entirely with no path, so the server default ($HOME) applies', () => {
    expect(listDirectoryOperation()).toMatchObject({ method: 'GET', path: '/api/fs/listdir' });
    expect(listDirectoryOperation('')).toMatchObject({ path: '/api/fs/listdir' });
  });

  it('sends an encoded absolute path', () => {
    expect(listDirectoryOperation('/home/a b/c&d').path)
      .toBe('/api/fs/listdir?path=%2Fhome%2Fa%20b%2Fc%26d');
  });

  it('carries no body, so the client sends no content-type on this read', () => {
    expect(listDirectoryOperation('/home')).not.toHaveProperty('body');
  });
});

describe('Track-scoped workspace file operations', () => {
  it('keeps the Track identity in the path and sends only a relative file query', () => {
    expect(readTrackWorkspaceFileOperation('track/1', 'docs/a b.md')).toMatchObject({
      method: 'GET',
      path: '/api/tracks/track%2F1/workspace/readfile?path=docs%2Fa%20b.md',
    });
    expect(trackWorkspaceRawFileUrl('track/1', 'img/a&b.png'))
      .toBe('/api/tracks/track%2F1/workspace/readfile-raw?path=img%2Fa%26b.png');
  });
});
