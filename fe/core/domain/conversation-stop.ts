import type { ApiFailure } from '../api/types.js';
import { classifyFailure, type FailureTable } from './failure-class.js';

/** Transient stop feedback is not a persisted terminal outcome. */
export type ConversationStopFeedback = Readonly<
  | { kind: 'requesting' }
  | { kind: 'stopping' }
  | { kind: 'unconfirmed' }
  | { kind: 'failed'; message: string }
>;

/**
 * What a failed `POST /planner/interrupt` says. `refused`: answered before anything was dispatched (403 not a
 * planner card, 404 no such card, 409 `planner_harness_dormant` no live session). Anything else, a lost answer
 * included, may follow a dispatched interrupt, so the stop is `unconfirmed`, never "failed".
 */
export const PLANNER_INTERRUPT_FAILURES: FailureTable<'refused' | 'unconfirmed'> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([403, 404]), is: 'refused' as const }),
    Object.freeze({ status: Object.freeze([409]), code: 'planner_harness_dormant', is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unconfirmed',
});

/** The stop row for a failed interrupt: a refusal shows the server's reason; anything else is the fixed unconfirmed state. */
export function stopFailureFeedback(failure: ApiFailure | null): ConversationStopFeedback {
  if (failure === null || classifyFailure(failure, PLANNER_INTERRUPT_FAILURES) === 'unconfirmed') return { kind: 'unconfirmed' };
  return { kind: 'failed', message: failure.message === '' ? 'The stop request was refused.' : failure.message };
}
