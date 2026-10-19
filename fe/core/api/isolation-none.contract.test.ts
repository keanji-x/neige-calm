import { describe, expectTypeOf, it } from 'vitest';
import type { Event, EventWire, TrackWire, WorkspaceIsolation } from './generated/wire.ts';

describe('workspace isolation wire contracts', () => {
  it('isolation_none_track_and_event_responses_require_the_projected_level', () => {
    expectTypeOf<TrackWire['workspace']['isolation']>().toEqualTypeOf<WorkspaceIsolation>();
    expectTypeOf<Extract<EventWire, { ev: 'track.updated' }>['data']['workspace']['isolation']>()
      .toEqualTypeOf<WorkspaceIsolation>();
    expectTypeOf<EventWire['ev']>().toEqualTypeOf<Event['ev']>();
    expectTypeOf<Exclude<EventWire, { ev: 'track.updated' }>>()
      .toEqualTypeOf<Exclude<Event, { ev: 'track.updated' }>>();
  });
});
