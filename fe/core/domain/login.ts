import type { FailureTable, WriteText } from './failure-class.js';

/**
 * What a failed `POST /api/auth/login` means. `credentials`: the 401 the server answers for a wrong username or
 * password, which the form words itself. `refused`: another answered 4xx, shown with its reason. `unknown`: no
 * trustworthy answer (lost, a 5xx, unreadable), shown as `LOGIN_TEXT.unknown`. A retry is safe: each success only
 * mints a new session.
 */
export type LoginFailure = 'credentials' | 'refused' | 'unknown';

export const LOGIN_FAILURES: FailureTable<LoginFailure> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze({ from: 400, to: 499 }), is: 'refused' as const }),
  ]),
  unauthorized: 'credentials',
  otherwise: 'unknown',
});

/** Neither sentence speaks of the connection: that is the global indicator's. */
export const LOGIN_TEXT: WriteText = Object.freeze({
  refused: 'Sign-in was refused.',
  unknown: 'Sign-in could not be completed. Try again.',
});
