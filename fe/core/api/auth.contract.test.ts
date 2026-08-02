import { describe, expect, expectTypeOf, it } from 'vitest';
import type { z } from 'zod';

import {
  loginRequestSchema,
  sessionIdentitySchema,
  whoamiOperation,
} from './auth.js';
import type { LoginRequest, SessionIdentity } from './auth.js';

describe('core/api auth contract', () => {
  it('pins the session request and response schema types bidirectionally', () => {
    expectTypeOf<z.infer<typeof loginRequestSchema>>().toEqualTypeOf<LoginRequest>();
    expectTypeOf<z.infer<typeof sessionIdentitySchema>>().toEqualTypeOf<SessionIdentity>();

    const compileOnly = false as boolean;
    if (compileOnly) {
      // @ts-expect-error -- every session identity field is required by the wire contract.
      const incomplete: SessionIdentity = { userId: 'owner' };
      void incomplete;
    }
  });

  it('freezes the whoami path and rejects malformed identities', () => {
    expect(whoamiOperation()).toMatchObject({ method: 'GET', path: '/api/auth/whoami' });
    expect(sessionIdentitySchema.safeParse({ userId: 'owner' }).success).toBe(false);
  });
});
