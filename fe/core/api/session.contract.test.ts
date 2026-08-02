import { describe, expect, expectTypeOf, it } from 'vitest';

import { resolveSessionProbe } from './session.js';
import type { SessionProbeState } from './session.js';

describe('core/api session classification contract', () => {
  it('keeps the pre-probe state explicit and content-free', () => {
    expectTypeOf<SessionProbeState<{ id: string }>['status']>()
      .toEqualTypeOf<'unknown' | 'authed' | 'unauthed' | 'error'>();
    expect(resolveSessionProbe<{ id: string }>(undefined)).toEqual({ status: 'unknown' });
  });

  it('maps only 401 to unauthed and preserves other failures as error', () => {
    expect(resolveSessionProbe({
      status: 'failed',
      error: { kind: 'unauthorized', status: 401, code: 'session_expired', message: 'expired' },
    })).toEqual({ status: 'unauthed' });
    expect(resolveSessionProbe({
      status: 'failed',
      error: { kind: 'transport', message: 'Transport request failed' },
    })).toEqual({
      status: 'error',
      error: { kind: 'transport', message: 'Transport request failed' },
    });
  });
});
