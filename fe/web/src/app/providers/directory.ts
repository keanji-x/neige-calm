// The `ListDirectory` port the directory browser takes as a prop, bound to the real transport; the
// composition layer is the one place that may hold both the operation and the browser's path-joining rule.

import {
  gitDiffOperation, gitStatusOperation, listDirectoryOperation, rawFileUrl, readFileOperation,
  readTrackWorkspaceFileOperation, toDirectoryListing, trackWorkspaceRawFileUrl,
  type CardFilesPort, type WorkspaceFilePort,
} from '../../../../core/domain/fs.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { joinDirectoryPath, type ListDirectory } from '../../ui/directory-browser/public.tsx';
import { runOperation } from './queries.ts';

/** `joinDirectoryPath` is passed in: the directory browser owns how a listing's rows are addressed, and the decoder must agree. */
export function createDirectoryLister(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
): ListDirectory {
  return async (path) => toDirectoryListing(
    await runOperation(transport, listDirectoryOperation(path), unauthorized),
    joinDirectoryPath,
  );
}

/** The same reads, as the port a card is handed: plain functions, no query cache in front of a filesystem that is already the source of truth. */
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

/** Reads for agent-authored Report links; never accepts an absolute root from the browser — the kernel applies containment. */
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
