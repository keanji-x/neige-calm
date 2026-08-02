import { describe, expectTypeOf, it } from 'vitest';

import type { UnauthorizedChannel } from './unauthorized.js';

describe('core/api unauthorized channel contract', () => {
  it('exposes subscription lifecycle and notification as explicit obligations', () => {
    expectTypeOf<UnauthorizedChannel['subscribe']>().returns.toEqualTypeOf<() => void>();
    expectTypeOf<UnauthorizedChannel['notify']>().returns.toEqualTypeOf<void>();
  });
});
