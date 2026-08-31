// The `ListDirectory` port `ui/directory-browser` (and the `DirectoryField`
// wrapper over it) takes as a prop, bound to the real transport.
//
// It sits in `app/providers` for the same reason `queries.ts` does: the picker
// is a `ui/` primitive that must not know a transport exists, and `features/**`
// may not import `app/**`, so the one place that can hold both the operation
// and the browser's own path-joining rule is the composition layer. The shell
// creates it once and hands it down as a plain function.

import { listDirectoryOperation, toDirectoryListing } from '../../../../core/domain/fs.ts';
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
