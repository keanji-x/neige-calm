import { QueryClient } from '@tanstack/react-query';
import { describe, expect, it } from 'vitest';

import { queryKeys } from '../providers/queries.ts';
import { trackLookupContext } from './track-lookup.ts';

describe('track lookup context', () => {
  it('finds a card in cached track details and returns null when it is absent', () => {
    const client = new QueryClient();
    client.setQueryData(queryKeys.trackDetail('track-1'), {
      track: {}, overlays: [], cards: [{ id: 'card-1' }],
    });
    client.setQueryData(queryKeys.areas(), [{ id: 'card-2' }]);

    const context = trackLookupContext(client);
    expect(context.findTrackOwningCard('card-1')).toBe('track-1');
    expect(context.findTrackOwningCard('missing')).toBeNull();
  });
});
