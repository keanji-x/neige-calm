// Filesystem reads the browser may make. `joinPath` is a parameter because `core/` may not
// import `web/src/ui/**`, where the path-joining rule lives; the app-layer adapter injects it.

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

/** `path` omitted starts the walk at the server's `$HOME`; the query key is left off rather than sent empty. */
export function listDirectoryOperation(path?: string): ApiOperation<DirectoryListingWire> {
  return {
    method: 'GET',
    path: path === undefined || path === ''
      ? '/api/fs/listdir'
      : `/api/fs/listdir?path=${encodeURIComponent(path)}`,
    responseSchema: directoryListingWireSchema,
  };
}

/** `GET /api/fs/readfile`. Text only: the kernel answers 400 for a binary or non-UTF-8 file. */
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

/** The URL an `<img>` reads an image file from; the browser fetches it itself, with the session cookie. */
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
 * Both sides of one changed file, as text. `head_text` is null for a file not in HEAD and
 * `working_text` null for a deleted one.
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
 * The filesystem reads a card may make, as a port: `systems/**` holds no transport, so the reads
 * arrive as injected functions.
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
