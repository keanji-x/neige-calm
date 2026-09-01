// Filesystem reads the browser is allowed to make: today exactly one, the
// read-only directory listing behind `GET /api/fs/listdir`.
//
// The kernel's wire shape and the shape `ui/directory-browser` consumes are
// deliberately different — the wire says `is_dir` and names an entry without
// placing it, the browser wants `isDirectory` and an absolute `path` per entry
// so a click can navigate without re-deriving where it came from. The
// translation lives here rather than at the call site so every end (browser,
// native, a test) decodes it once and the same way.
//
// **Why `joinPath` is a parameter.** Assembling `<parent>/<name>` is owned by
// `ui/directory-browser` (`joinDirectoryPath`), which also owns the trailing
// slash and root-path rules the picker's own input relies on; a second copy
// here would be a duplicate definition of the same rule that could drift.
// `core/` may not import `web/src/ui/**` (`core-no-web-layers`), so the owner's
// function is injected by the app-layer adapter instead — see
// `web/src/app/providers/directory.ts`, the only production caller, which
// passes `joinDirectoryPath` itself.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';

export const directoryEntryWireSchema = z.object({
  name: z.string(),
  is_dir: z.boolean(),
});
export type DirectoryEntryWire = z.infer<typeof directoryEntryWireSchema>;

/** `parent` is `null` at the filesystem root and only there. */
export const directoryListingWireSchema = z.object({
  path: z.string(),
  parent: z.string().nullable(),
  entries: z.array(directoryEntryWireSchema),
});
export type DirectoryListingWire = z.infer<typeof directoryListingWireSchema>;

/**
 * `path` omitted starts the walk at the server's `$HOME`; that default is the
 * kernel's, not ours, so the query key is left off entirely rather than sent
 * empty (`?path=` is the same branch server-side, but an absent key is what
 * "we have no opinion" actually means).
 */
export function listDirectoryOperation(path?: string): ApiOperation<DirectoryListingWire> {
  return {
    method: 'GET',
    path: path === undefined || path === ''
      ? '/api/fs/listdir'
      : `/api/fs/listdir?path=${encodeURIComponent(path)}`,
    responseSchema: directoryListingWireSchema,
  };
}

export type DirectoryListingEntry = Readonly<{
  name: string;
  path: string;
  isDirectory: boolean;
}>;

export type DirectoryListingView = Readonly<{
  path: string;
  parent: string | null;
  entries: readonly DirectoryListingEntry[];
}>;

/** Structurally the `DirectoryListing` `ui/directory-browser` renders. */
export function toDirectoryListing(
  wire: DirectoryListingWire,
  joinPath: (parent: string, name: string) => string,
): DirectoryListingView {
  return {
    path: wire.path,
    parent: wire.parent,
    entries: wire.entries.map((entry) => ({
      name: entry.name,
      path: joinPath(wire.path, entry.name),
      isDirectory: entry.is_dir,
    })),
  };
}
