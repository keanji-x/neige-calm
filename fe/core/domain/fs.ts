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

/**
 * `GET /api/fs/readfile`. Text only: the kernel answers 400 for a binary or
 * non-UTF-8 file rather than returning bytes, and `truncated` says the read hit
 * the size cap — a viewer that did not print that would be silently showing a
 * prefix as though it were the file.
 */
export const readFileWireSchema = z.object({
  path: z.string(),
  size: z.number(),
  text: z.string(),
  truncated: z.boolean(),
});
export type ReadFileWire = z.infer<typeof readFileWireSchema>;

export function readFileOperation(path: string): ApiOperation<ReadFileWire> {
  return {
    method: 'GET',
    path: `/api/fs/readfile?path=${encodeURIComponent(path)}`,
    responseSchema: readFileWireSchema,
  };
}

/**
 * The URL an `<img>` reads an image file from. Not an `ApiOperation`: the
 * browser fetches it itself, with the session cookie, and there is no JSON to
 * decode — the whole value of the endpoint is that it is addressable.
 */
export function rawFileUrl(path: string): string {
  return `/api/fs/readfile-raw?path=${encodeURIComponent(path)}`;
}

/** A Track-scoped read: the kernel, not the browser, owns the workspace root. */
export function readTrackWorkspaceFileOperation(
  trackId: string,
  path: string,
): ApiOperation<ReadFileWire> {
  return {
    method: 'GET',
    path: `/api/tracks/${encodeURIComponent(trackId)}/workspace/readfile?path=${encodeURIComponent(path)}`,
    responseSchema: readFileWireSchema,
  };
}

/** Raw image counterpart to {@link readTrackWorkspaceFileOperation}. */
export function trackWorkspaceRawFileUrl(trackId: string, path: string): string {
  return `/api/tracks/${encodeURIComponent(trackId)}/workspace/readfile-raw?path=${encodeURIComponent(path)}`;
}

/** The only filesystem capabilities an agent-authored Report file may use. */
export type WorkspaceFilePort = Readonly<{
  readFile: (path: string) => Promise<ReadFileWire>;
  rawUrl: (path: string) => string;
}>;

/** `status` is the kernel's word: modified / added / deleted / untracked / renamed. */
export const gitChangedFileWireSchema = z.object({
  path: z.string(),
  status: z.string(),
  old_path: z.string().optional(),
});
export type GitChangedFileWire = z.infer<typeof gitChangedFileWireSchema>;

export const gitStatusWireSchema = z.object({
  repo_root: z.string(),
  files: z.array(gitChangedFileWireSchema),
});
export type GitStatusWire = z.infer<typeof gitStatusWireSchema>;

export function gitStatusOperation(path: string): ApiOperation<GitStatusWire> {
  return {
    method: 'GET',
    path: `/api/fs/gitstatus?path=${encodeURIComponent(path)}`,
    responseSchema: gitStatusWireSchema,
  };
}

/**
 * Both sides of one changed file, as text. The kernel sends the two versions
 * rather than a unified diff because the viewer renders them side by side and
 * would otherwise have to parse a patch back into them.
 *
 * `head_text` is null for a file that is not in HEAD (added / untracked) and
 * `working_text` is null for a deleted one; neither is an error.
 */
export const gitDiffWireSchema = z.object({
  path: z.string(),
  status: z.string(),
  head_text: z.string().nullable(),
  working_text: z.string().nullable(),
  truncated: z.boolean(),
});
export type GitDiffWire = z.infer<typeof gitDiffWireSchema>;

export function gitDiffOperation(path: string, oldPath?: string): ApiOperation<GitDiffWire> {
  const query = oldPath === undefined || oldPath === ''
    ? `path=${encodeURIComponent(path)}`
    : `path=${encodeURIComponent(path)}&old_path=${encodeURIComponent(oldPath)}`;
  return {
    method: 'GET',
    path: `/api/fs/gitdiff?${query}`,
    responseSchema: gitDiffWireSchema,
  };
}

/**
 * The filesystem reads a card may make, as a port.
 *
 * A card is rendered deep inside `systems/**`, which holds no transport and may
 * not acquire one — so the reads arrive as injected functions, built once at
 * the composition layer (`app/composition.ts`) from the same transport and the
 * same 401 channel every other read in the app uses. Declared here, in `core`,
 * because that is the one place both ends may import from.
 */
export type CardFilesPort = Readonly<{
  listDirectory: (path: string) => Promise<DirectoryListingWire>;
  readFile: (path: string) => Promise<ReadFileWire>;
  gitStatus: (path: string) => Promise<GitStatusWire>;
  gitDiff: (path: string, oldPath?: string) => Promise<GitDiffWire>;
  /** The `<img src>` for an image file — see `rawFileUrl`. */
  rawUrl: (path: string) => string;
}>;

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
