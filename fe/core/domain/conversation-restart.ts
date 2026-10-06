import { z } from 'zod';

import type { ApiFailure, ApiOperation } from '../api/types.js';
import { refusalText, type FailureTable, type NotSentError, type WriteFailure, type WriteText } from './failure-class.js';

/**
 * `POST /planner/restart` (#2192): a fresh session on a new thread. The transcript is kept, and the messages the old
 * session never delivered are queued on the new one. There is deliberately no operation for `/planner/reset`, which
 * erases the transcript: nothing in the front end offers it.
 */
export function restartPlannerOperation(cardId: string): ApiOperation<{ new_thread_id: string }> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/planner/restart`,
    responseSchema: z.object({ card_id: z.string(), new_thread_id: z.string() }),
  };
}

/**
 * What a failed restart says. `refused`: the documented answers given before anything starts (400 a path that does not
 * parse, 403 not a planner card, 404 no such card or track, 409 the provider cannot start now). A 5xx, a lost answer or
 * an undocumented status may follow a started session, so it is `unknown`: the run state read after it says which.
 */
export const PLANNER_RESTART_FAILURES: FailureTable<WriteFailure> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([400, 403, 404, 409]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const RESTART_TEXT: WriteText = Object.freeze({
  refused: 'The fresh session was not started.',
  unknown: 'Starting a fresh session is unconfirmed.',
});

/** The one way out of a session that cannot go on; the same words wherever it is offered. */
export const RESTART_ACTION = 'Start a fresh session';

/**
 * A send refused with `planner_harness_dormant`: the session cannot be resumed. Keyed on that code, never on the
 * server's words. The words are back in the composer below the strip.
 */
export const DORMANT_NOTICE = 'Not sent. This conversation’s session can’t be resumed. Your history is kept and your message is still below.';

/** A paused (`wedged`) conversation, where Send is disabled until a fresh session starts. */
export const PAUSED_NOTICE = 'This conversation’s session is stuck. A fresh session keeps your history and queued messages.';

/** A restart that was answered: the new session does not carry the old thread's context. */
export const RESTARTED_NOTICE = 'Fresh session started. Earlier messages stay here, but the assistant won’t remember them unless you mention them.';

/** The sentence for a failed restart: the server's reason for a refusal, else the fixed unconfirmed one. */
export function restartFailureText(failure: ApiFailure | NotSentError | null): string {
  return refusalText(failure, PLANNER_RESTART_FAILURES, RESTART_TEXT.refused) ?? RESTART_TEXT.unknown;
}
