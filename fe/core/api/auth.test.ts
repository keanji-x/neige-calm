import { describe, expect, it } from 'vitest';

import { sessionIdentitySchema } from './auth.js';

describe('core/api auth behavior', () => {
  it('rejects incomplete and unknown session identity fields', () => {
    expect(sessionIdentitySchema.safeParse({ userId: 'owner' }).success).toBe(false);
    expect(sessionIdentitySchema.safeParse({
      userId: 'owner',
      displayName: 'Owner',
      role: 'admin',
      sessionId: 'session-1',
      extra: 1,
    }).success).toBe(false);
  });
});
