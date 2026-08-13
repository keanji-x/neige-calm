import { QueryClient } from '@tanstack/react-query';
import { describe, expect, it } from 'vitest';

import { queryKeys } from '../providers/queries.ts';
import { waveLookupContext } from './wave-lookup.ts';

describe('wave lookup context', () => {
  it('finds a card in cached wave details and returns null when it is absent', () => {
    const client = new QueryClient();
    client.setQueryData(queryKeys.waveDetail('wave-1'), {
      wave: {}, overlays: [], cards: [{ id: 'card-1' }],
    });
    client.setQueryData(queryKeys.coves(), [{ id: 'card-2' }]);

    const context = waveLookupContext(client);
    expect(context.findWaveOwningCard('card-1')).toBe('wave-1');
    expect(context.findWaveOwningCard('missing')).toBeNull();
  });
});
