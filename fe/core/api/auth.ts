import { z } from 'zod';

import type { ApiAbortSignal, ApiOperation } from './types.js';

export const sessionIdentitySchema = z.strictObject({
  userId: z.string(),
  displayName: z.string(),
  role: z.string(),
  sessionId: z.string(),
});
export type SessionIdentity = z.infer<typeof sessionIdentitySchema>;

export const loginRequestSchema = z.strictObject({
  username: z.string(),
  password: z.string(),
});
export type LoginRequest = z.infer<typeof loginRequestSchema>;

export function whoamiOperation(signal?: ApiAbortSignal): ApiOperation<SessionIdentity> {
  return { method: 'GET', path: '/api/auth/whoami', responseSchema: sessionIdentitySchema,
    ...(signal === undefined ? {} : { signal }) };
}

export function loginOperation(body: LoginRequest): ApiOperation<SessionIdentity> {
  return { method: 'POST', path: '/api/auth/login', body, responseSchema: sessionIdentitySchema };
}
