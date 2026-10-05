import type { ApiFailure } from '../api/types.js';
import { refusalText, type FailureTable, type NotSentError, type WriteFailure } from './failure-class.js';

/** Transient stop feedback is not a persisted terminal outcome. */
export type ConversationStopFeedback = Readonly<
  | { kind: 'requesting' }
  | { kind: 'stopping' }
  | { kind: 'unconfirmed' }
  | { kind: 'failed'; message: string }
>;

/**
 * What a failed `POST /planner/interrupt` says. `refused`: the documented answers given before anything is dispatched
 * (403 not a planner card, 404 no such card, 409 `planner_harness_dormant` no live session). A lost or unreadable
 * answer, or a 5xx (what a failure after dispatch answers), may follow a dispatched interrupt. Any other status is
 * not documented for this route, so it is read the same conservative way: the stop is `unknown`, shown as
 * unconfirmed, never "failed".
 */
export const PLANNER_INTERRUPT_FAILURES: FailureTable<WriteFailure> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([403, 404]), is: 'refused' as const }),
    Object.freeze({ status: Object.freeze([409]), code: 'planner_harness_dormant', is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

/** The stop row for a failed interrupt: a refusal shows the server's reason; anything else is the fixed unconfirmed state. */
export function stopFailureFeedback(failure: ApiFailure | NotSentError | null): ConversationStopFeedback {
  const message = refusalText(failure, PLANNER_INTERRUPT_FAILURES, 'The response was not stopped.');
  return message === null ? { kind: 'unconfirmed' } : { kind: 'failed', message };
}
