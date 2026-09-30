// The `@` search port (#1881): `GET /api/areas/{area_id}/mentions`, read fresh on every call.
// No query cache: the server reads SQL per request so a new tag or a rename shows on the next
// keystroke, and a client cache would undo that.

import { useMemo } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { mentionQueryOf, mentionSuggestionsOf, mentionsOperation, type MentionSearch } from '../../../../core/domain/mentions.ts';
import { runOperation } from './queries.ts';

/** `trackId` is the track the message is written in, or `null` before there is one. */
export function mentionSearchOf(
  transport: ApiTransportPort, unauthorized: UnauthorizedChannel, areaId: string, trackId: string | null,
): MentionSearch {
  return async (typed, signal) => {
    const query = mentionQueryOf(typed);
    return mentionSuggestionsOf(
      await runOperation(transport, { ...mentionsOperation(areaId, query.text, trackId), signal }, unauthorized),
      query,
    );
  };
}

/** The search for one Area's `@` menu, or `null` when the composer's reader is not that Area's Planner. */
export function useMentionSearch(
  transport: ApiTransportPort, unauthorized: UnauthorizedChannel, areaId: string | null, trackId: string | null,
): MentionSearch | null {
  return useMemo(
    () => areaId === null ? null : mentionSearchOf(transport, unauthorized, areaId, trackId),
    [transport, unauthorized, areaId, trackId],
  );
}
