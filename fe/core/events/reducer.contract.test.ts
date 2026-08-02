import { describe, expectTypeOf, it } from 'vitest';

import type { EventEffect, EventReduction, EventState } from './reducer.js';

describe('event reducer contract', () => {
  it('pins state, reduction, and ordered effect vocabulary', () => {
    expectTypeOf<EventState>().toEqualTypeOf<Readonly<{
      cursor: number | null;
      syncEventVersion: number | null;
    }>>();
    expectTypeOf<EventReduction['effects']>().toEqualTypeOf<readonly EventEffect[]>();
    expectTypeOf<EventEffect['type']>().toEqualTypeOf<
      'persist-cursor' | 'invalidate' | 'remove' | 'write-through' | 'clear-cache' | 'reconnect'
    >();
  });
});
