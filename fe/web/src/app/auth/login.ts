import { loginOperation, type SessionIdentity } from '../../../../core/api/auth.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { ApiError, runOperation } from '../providers/queries.ts';
import type { RecoverySession } from '../../systems/recovery/session.ts';

/** Login treats rejected credentials as an expected result and does not broadcast its 401. */
export async function loginWithTransport(
  transport: ApiTransportPort, username: string, password: string, signal: AbortSignal,
): Promise<SessionIdentity | null> {
  try {
    signal.throwIfAborted();
    const identity = await runOperation(transport, { ...loginOperation({ username, password }), signal }, undefined);
    signal.throwIfAborted();
    return identity;
  } catch (cause: unknown) {
    if (cause instanceof ApiError && cause.failure.kind === 'unauthorized') return null;
    throw cause;
  }
}

/** The form's cancellation spans the login POST and the same session owner's proof. */
export async function loginForRecovery(transport: ApiTransportPort, session: RecoverySession,
  username: string, password: string, signal: AbortSignal): Promise<SessionIdentity | null> {
  const identity = await loginWithTransport(transport, username, password, signal);
  signal.throwIfAborted();
  return identity === null ? null : session.verifyNewSession(identity.sessionId, signal);
}
