// The `ListDirectory` port `ui/directory-browser` (and the `DirectoryField`
// wrapper over it) takes as a prop, bound to the real transport.
//
// It sits in `app/providers` for the same reason `queries.ts` does: the picker
// is a `ui/` primitive that must not know a transport exists, and `features/**`
// may not import `app/**`, so the one place that can hold both the operation
// and the browser's own path-joining rule is the composition layer. Each route
// that needs a picker creates it and hands it down as a plain function — the
// shell used to, when it owned the New track dialog (#1211 made that a route).

import {
  gitDiffOperation, gitStatusOperation, listDirectoryOperation, rawFileUrl, readFileOperation,
  readTrackWorkspaceFileOperation, toDirectoryListing, trackWorkspaceRawFileUrl,
  type CardFilesPort, type WorkspaceFilePort,
} from '../../../../core/domain/fs.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { joinDirectoryPath, type ListDirectory } from '../../ui/directory-browser/public.tsx';
import { runOperation } from './queries.ts';

/**
 * `joinDirectoryPath` is passed in rather than re-implemented in `core`: the
 * directory browser owns how a listing's rows are addressed, and the decoder
 * that feeds it must use that same rule or a click would navigate somewhere the
 * input bar does not agree with. See the header of `core/domain/fs.ts`.
 *
 * Failures propagate as the rejected promise the browser already renders — it
 * shows `reason.message` for an `Error`, and `ApiError` is one.
 */
export function createDirectoryLister(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
): ListDirectory {
  return async (path) => toDirectoryListing(
    await runOperation(transport, listDirectoryOperation(path), unauthorized),
    joinDirectoryPath,
  );
}

/**
 * The same reads, as the port a card is handed.
 *
 * It sits beside `createDirectoryLister` for the same reason that one is here:
 * a card is rendered inside `systems/**`, which may not reach a transport, so
 * the composition layer is the only place that can hold both. What a card gets
 * is plain functions — no query client, no cache — because a card's reads are
 * driven by its own state (which file, which tab) rather than by a route, and
 * folding them into TanStack keys would put a second cache in front of a
 * filesystem that is already the source of truth.
 *
 * Failures propagate as the rejected promise each caller renders; `ApiError` is
 * an `Error`, so a pane can print `reason.message` directly.
 */
export function createCardFilesPort(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
): CardFilesPort {
  return Object.freeze({
    listDirectory: (path) => runOperation(transport, listDirectoryOperation(path), unauthorized),
    readFile: (path) => runOperation(transport, readFileOperation(path), unauthorized),
    gitStatus: (path) => runOperation(transport, gitStatusOperation(path), unauthorized),
    gitDiff: (path, oldPath) => runOperation(transport, gitDiffOperation(path, oldPath), unauthorized),
    rawUrl: rawFileUrl,
  });
}

/**
 * Reads for agent-authored Report links. Unlike a user-created file Card, this
 * port never accepts an absolute root from the browser: the Track id reaches a
 * kernel endpoint that loads the persisted workspace and applies containment
 * after canonicalization.
 */
export function createTrackWorkspaceFilesPort(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
  trackId: string,
): WorkspaceFilePort {
  return Object.freeze({
    readFile: (path) => runOperation(
      transport,
      readTrackWorkspaceFileOperation(trackId, path),
      unauthorized,
    ),
    rawUrl: (path) => trackWorkspaceRawFileUrl(trackId, path),
  });
}
