import type { QueryClient } from '@tanstack/react-query';

import type { InvalidationContext } from '../../../../core/events/invalidation-plan.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';

/** Resolves card-scoped events from track details already present in the cache. */
export function trackLookupContext(client: Pick<QueryClient, 'getQueriesData'>): InvalidationContext {
  return {
    findTrackOwningCard(cardId) {
      for (const [key, detail] of client.getQueriesData<TrackDetailWire>({ queryKey: ['track'] })) {
        if (detail?.cards.some((card) => card.id === cardId)) {
          const trackId = key[1];
          return typeof trackId === 'string' ? trackId : null;
        }
      }
      return null;
    },
  };
}
