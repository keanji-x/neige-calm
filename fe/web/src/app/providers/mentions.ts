// The `@` search port (#1881): `GET /api/areas/{area_id}/mentions`, read fresh on every call.
// No query cache: the server reads SQL per request so a new tag or a rename shows on the next
// keystroke, and a client cache would undo that.

import { useMemo } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { mentionQueryOf, mentionSuggestionsOf, mentionsOperation, pluginMentionSuggestions, type MentionSearch, type MentionSuggestion } from '../../../../core/domain/mentions.ts';
import { pluginsOperation } from '../../../../core/domain/plugins.ts';
import { runOperation } from './queries.ts';

/** `trackId` is the track the message is written in, or `null` before there is one. */
export function mentionSearchOf(
  transport: ApiTransportPort, unauthorized: UnauthorizedChannel, areaId: string | null, trackId: string | null,
): MentionSearch {
  return async (typed, signal) => {
    const query = mentionQueryOf(typed);
    const sources: Promise<readonly MentionSuggestion[]>[] = [];
    if (areaId !== null && query.kind !== 'plugin') sources.push(runOperation(transport,
      { ...mentionsOperation(areaId, query.text, trackId), signal }, unauthorized)
      .then(candidates => mentionSuggestionsOf(candidates, query)));
    if (query.kind === null || query.kind === 'plugin') sources.push(runOperation(transport, { ...pluginsOperation(), signal }, unauthorized)
      .then(catalog => pluginMentionSuggestions(catalog, typed)));
    const results = await Promise.allSettled(sources);
    const failed = results.find(result => result.status === 'rejected');
    if (!results.some(result => result.status === 'fulfilled') && failed?.status === 'rejected') throw failed.reason;
    return results.flatMap(result => result.status === 'fulfilled' ? [...result.value] : []);
  };
}

/** Plugin documentation everywhere; Area report references only for that Area's Planner. */
export function useMentionSearch(
  transport: ApiTransportPort, unauthorized: UnauthorizedChannel, areaId: string | null, trackId: string | null,
): MentionSearch {
  return useMemo(
    () => mentionSearchOf(transport, unauthorized, areaId, trackId),
    [transport, unauthorized, areaId, trackId],
  );
}
