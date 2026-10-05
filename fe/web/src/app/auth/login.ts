import { loginOperation, type SessionIdentity } from '../../../../core/api/auth.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { ApiError, classifyFailure, refusedText } from '../../../../core/domain/failure-class.ts';
import { LOGIN_FAILURES, LOGIN_TEXT } from '../../../../core/domain/login.ts';
import { runOperation } from '../providers/queries.ts';
import type { RecoverySession } from '../../systems/recovery/session.ts';

/**
 * Login treats rejected credentials as an expected result and does not broadcast its 401. Any other failure is
 * thrown as the sentence its `LOGIN_FAILURES` class gives it, never as raw transport text.
 */
export async function loginWithTransport(
  transport: ApiTransportPort, username: string, password: string, signal: AbortSignal,
): Promise<SessionIdentity | null> {
  try {
    signal.throwIfAborted();
    const identity = await runOperation(transport, { ...loginOperation({ username, password }), signal }, undefined);
    signal.throwIfAborted();
    return identity;
  } catch (cause: unknown) {
    if (!(cause instanceof ApiError)) throw cause;
    const kind = classifyFailure(cause.failure, LOGIN_FAILURES);
    if (kind === 'credentials') return null;
    throw new Error(kind === 'refused' ? refusedText(cause.failure, LOGIN_TEXT.refused) : LOGIN_TEXT.unknown, { cause });
  }
}

/** The form's cancellation spans the login POST and the same session owner's proof. */
export async function loginForRecovery(transport: ApiTransportPort, session: RecoverySession,
  username: string, password: string, signal: AbortSignal): Promise<SessionIdentity | null> {
  const attempt = session.beginAuthentication(signal);
  try {
    const identity = await loginWithTransport(transport, username, password, attempt.signal);
    return identity === null ? null : await attempt.verify(identity.sessionId);
  } finally { attempt.cancel(); }
}
