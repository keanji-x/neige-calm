import { describe, expect, it } from 'vitest';

import { decodeEventFrame, eventSubscriptionFrame } from './protocol.js';

describe('event protocol behavior', () => {
  it('reports non-object inputs through the failed decode branch', () => {
    expect(decodeEventFrame('not-an-envelope')).toEqual({
      status: 'failed',
      error: {
        kind: 'decode',
        message: 'Event frame must be an object',
        cause: 'not-an-envelope',
      },
    });
  });

  it('always publishes a since cursor and uses zero for cold starts', () => {
    expect(eventSubscriptionFrame(['*'], null)).toEqual({ sub: ['*'], since: 0 });
    expect(eventSubscriptionFrame(['wave:w1'], 42)).toEqual({ sub: ['wave:w1'], since: 42 });
  });
});
