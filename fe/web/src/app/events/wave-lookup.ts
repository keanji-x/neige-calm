import type { QueryClient } from '@tanstack/react-query';

import type { InvalidationContext } from '../../../../core/events/invalidation-plan.ts';
import type { WaveDetailWire } from '../../../../core/domain/wave.ts';

/** Resolves card-scoped events from wave details already present in the cache. */
export function waveLookupContext(client: Pick<QueryClient, 'getQueriesData'>): InvalidationContext {
  return {
    findWaveOwningCard(cardId) {
      for (const [key, detail] of client.getQueriesData<WaveDetailWire>({ queryKey: ['wave'] })) {
        if (detail?.cards.some((card) => card.id === cardId)) {
          const waveId = key[1];
          return typeof waveId === 'string' ? waveId : null;
        }
      }
      return null;
    },
  };
}
