import type { UseQueryResult } from '@tanstack/react-query';

import { readErrorText } from '../../../../core/domain/read-failure.ts';

/** A first-read retry clears Query's error temporarily; its failure timestamp
 * survives until success. Keep recovery controls mounted through that pending
 * phase without mirroring request state or showing an error on the first load. */
export function hasReadFailure(query: Pick<UseQueryResult, 'isError' | 'isPending' | 'errorUpdatedAt'>): boolean {
  return query.isError || (query.isPending && query.errorUpdatedAt > 0);
}

/** The workspace reads a failure sentence is made from; `Workspace` carries them. */
type WorkspaceReads = Readonly<{
  areas: readonly unknown[];
  areasError: Error | null;
  trackErrorsByArea: ReadonlyMap<string, Error>;
  overlaysError: Error | null;
}>;

/**
 * The workspace's one read-failure sentence: the area list first (whether a list is already on screen decides
 * between refreshed and unavailable), then the first area whose tracks could not be read. `null` when both read.
 */
export function workspaceReadErrorText(workspace: WorkspaceReads): string | null {
  if (workspace.areasError !== null) {
    return readErrorText(workspace.areasError, workspace.areas.length > 0 ? 'Areas could not be refreshed.' : 'Areas are unavailable.');
  }
  const tracksError = workspace.trackErrorsByArea.values().next().value;
  return tracksError === undefined ? null : readErrorText(tracksError, 'Tracks are unavailable.');
}

/** The workspace-wide track activity read, as its failure sentence, or `null` when it read. */
export function workspaceActivityErrorText(workspace: WorkspaceReads): string | null {
  return workspace.overlaysError === null ? null : readErrorText(workspace.overlaysError, 'Track activity is unavailable.');
}
