// #2516: the port a Claude card is handed for its Update and Resume, bound to the app's transport. The composition
// layer is the one place that holds the operations, the 401 channel and the query cache together.

import type { QueryClient } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  claudeInstalledVersionOperation, refreshClaudeInstalledVersionOperation, restartClaudeCardOperation,
  type ClaudeCardPort,
} from '../../../../core/domain/claude-card.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';
import { queryKeys, runOperation } from './queries.ts';

/**
 * A restart answers with the card as it now is: it replaces the card in its track's cached detail at once, so the head
 * drops its action and the surface attaches to the new runtime without waiting for the event's refetch.
 */
export function createClaudeCardPort(
  transport: ApiTransportPort,
  unauthorized: UnauthorizedChannel,
  client: QueryClient,
): ClaudeCardPort {
  return Object.freeze({
    installedVersion: (signal) => runOperation(transport, claudeInstalledVersionOperation(signal), unauthorized),
    refreshInstalledVersion: async () => {
      await runOperation(transport, refreshClaudeInstalledVersionOperation(), unauthorized);
    },
    restart: async (cardId) => {
      const card = await runOperation(transport, restartClaudeCardOperation(cardId), unauthorized);
      client.setQueryData(queryKeys.trackDetail(card.track_id), (previous: TrackDetailWire | undefined) => (
        previous === undefined ? previous : {
          ...previous,
          cards: previous.cards.map((existing) => (existing.id === card.id ? card : existing)),
        }
      ));
      void client.invalidateQueries({ queryKey: queryKeys.trackDetail(card.track_id) });
      return card;
    },
  });
}
