import type { ApiFailure } from '../api/types.js';
import { ApiError, classifyFailure, failureReason, type FailureTable } from './failure-class.js';

/**
 * What a failed read means to the reader: `refused`, the server answered and said why (its reason is worth showing);
 * `unavailable`, the read did not get an answer it can explain (a lost or unreadable answer, a server fault, a timeout,
 * a rate limit, or a signed-out session, which the sign-in flow handles).
 */
export type ReadClass = 'refused' | 'unavailable';

/**
 * The one table every read shares. A 4xx is the server refusing this read for a reason it states, except 401
 * (unauthorized, the session's to handle), 408 and 429 (the same read may pass later). 5xx, transport and decode
 * failures are `unavailable`: their text is the server's or the browser's internals, never the reader's.
 */
export const READ_FAILURES: FailureTable<ReadClass> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([408, 429]), is: 'unavailable' as const }),
    Object.freeze({ status: Object.freeze({ from: 400, to: 499 }), is: 'refused' as const }),
  ]),
  unauthorized: 'unavailable',
  otherwise: 'unavailable',
});

/** The failure a rejected read carries, or `null` for any other thrown value (which reads as `unavailable`). */
export function readFailureOf(error: unknown): ApiFailure | null {
  return error instanceof ApiError ? error.failure : null;
}

/**
 * The one sentence rule for a failed read: the site's fixed `sentence`, followed by the server's reason when the
 * server refused the read. Nothing else the failure carries reaches the screen. The sentence is the site's own and
 * says what could not be read; the reason never replaces it.
 */
export function readFailureText(failure: ApiFailure | null, sentence: string): string {
  if (failure === null || classifyFailure(failure, READ_FAILURES) !== 'refused') return sentence;
  const reason = failureReason(failure).trim();
  return reason === '' ? sentence : `${sentence} ${reason}`;
}

/** {@link readFailureText} of a rejected read's error. */
export function readErrorText(error: unknown, sentence: string): string {
  return readFailureText(readFailureOf(error), sentence);
}
