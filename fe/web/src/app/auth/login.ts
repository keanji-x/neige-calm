import { loginOperation, type SessionIdentity } from '../../../../core/api/auth.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { ApiError, runOperation } from '../providers/queries.ts';

/** Login treats rejected credentials as an expected result and does not broadcast its 401. */
export async function loginWithTransport(
  transport: ApiTransportPort, username: string, password: string,
): Promise<SessionIdentity | null> {
  try {
    return await runOperation(transport, loginOperation({ username, password }), undefined);
  } catch (cause: unknown) {
    if (cause instanceof ApiError && cause.failure.kind === 'unauthorized') return null;
    throw cause;
  }
}
