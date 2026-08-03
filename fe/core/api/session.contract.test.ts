import { describe, expectTypeOf, it } from 'vitest';

import type { SessionProbeState } from './session.js';

describe('core/api session classification contract', () => {
  it('keeps the pre-probe state explicit and content-free', () => {
    expectTypeOf<SessionProbeState<{ id: string }>['status']>()
      .toEqualTypeOf<'unknown' | 'authed' | 'unauthed' | 'error'>();
  });
});
