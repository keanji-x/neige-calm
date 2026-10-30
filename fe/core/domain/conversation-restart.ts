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
export const RESTART_ACTION = 'Start a fresh session, keep history';

/**
 * A send refused with `planner_harness_dormant`: the session cannot be resumed. Keyed on that code, never on the
 * server's words. The words are back in the composer below the strip.
 */
export const DORMANT_NOTICE = 'Not sent. This conversation’s session can’t be resumed. Your history is kept and your message is still below.';

const recoveryCauseSchema = z.enum(['unknown_outcome', 'wedged', 'native_completion_pending', 'terminal_provider_error']);
export const runRecoverySchema = z.discriminatedUnion('action', [
  z.object({ cause: recoveryCauseSchema, action: z.literal('restart_session') }),
  z.object({ cause: recoveryCauseSchema, action: z.literal('abandon_session'), request: z.object({
    carrier_id: z.string(), submission_id: z.string(), attempt_id: z.string(), revision: z.string(),
  }) }),
  z.object({ cause: recoveryCauseSchema, action: z.literal('recover_session'), request: z.object({
    carrier_id: z.string(), submission_id: z.string(), attempt_id: z.string(), revision: z.string(),
  }) }),
  z.object({ cause: recoveryCauseSchema, action: z.literal('held'), reason: z.enum(['pending_inputs', 'native_completion_unavailable', 'cleanup_required', 'identity_unconfirmed', 'terminal_evidence_unavailable', 'human_authorization_required']) }),
]);
export type RunRecovery = z.infer<typeof runRecoverySchema>;

/** Only a declared action licenses a fresh-session offer. */
export function offersFreshSession(recovery: RunRecovery | null): boolean {
  return recovery?.action === 'restart_session' || recovery?.action === 'abandon_session' || recovery?.action === 'recover_session';
}

function recoveryReason(recovery: RunRecovery): string {
  if (recovery.action === 'held' && recovery.reason === 'terminal_evidence_unavailable') return 'Waiting to verify the failed execution has ended. Saved messages remain queued.';
  if (recovery.action === 'held' && recovery.reason === 'human_authorization_required') return 'Recovery requires current permission to run this conversation. Saved messages remain queued.';
  if (recovery.cause === 'terminal_provider_error') return 'The provider ended the previous execution with an error. The failed request won’t be sent again.';
  if (recovery.action === 'held' && recovery.reason === 'pending_inputs') return 'Some queued inputs still belong to the previous session. Recovery is held to keep them from being lost or repeated.';
  if (recovery.action === 'held' && recovery.reason === 'identity_unconfirmed') return 'This conversation’s session could not be confirmed after an earlier launch, so nothing more is sent. Saved messages are kept.';
  if (recovery.cause === 'native_completion_pending') return 'Waiting to verify the original native completion. Saved messages remain queued.';
  return recovery.cause === 'unknown_outcome'
    ? 'The previous request’s outcome is unknown. It won’t be sent again.'
    : 'This conversation’s session is stuck.';
}

/** Declared recovery explains the status; other stalls retain their runtime reason. */
export function recoveryStatusNotice(recovery: RunRecovery | null, stalledReason: string | null): Readonly<{ heading: string; reason: string }> {
  if (recovery?.action === 'held' && (recovery.reason === 'terminal_evidence_unavailable' || recovery.reason === 'human_authorization_required')) return { heading: 'Paused', reason: recoveryReason(recovery) };
  if (recovery?.cause === 'terminal_provider_error') return { heading: 'Provider error', reason: recoveryReason(recovery) };
  if (recovery?.cause === 'native_completion_pending') return { heading: 'Completion unconfirmed', reason: stalledReason ?? recoveryReason(recovery) };
  return recovery?.cause === 'unknown_outcome'
    ? { heading: 'Outcome unknown', reason: recoveryReason(recovery) }
    : { heading: 'Paused', reason: stalledReason ?? 'This conversation is stuck.' };
}

export function recoveryNotice(recovery: RunRecovery): string {
  const reason = recoveryReason(recovery);
  return `${reason} History stays here; a fresh model session starts without that context.`;
}

/** A restart that was answered: the new session does not carry the old thread's context. */
export const RESTARTED_NOTICE = 'Fresh session started. Earlier messages stay here, but the assistant won’t remember them unless you mention them.';

/** The sentence for a failed restart: the server's reason for a refusal, else the fixed unconfirmed one. */
export function restartFailureText(failure: ApiFailure | NotSentError | null): string {
  return refusalText(failure, PLANNER_RESTART_FAILURES, RESTART_TEXT.refused) ?? RESTART_TEXT.unknown;
}

/** Recovery of canonical Unknown is explicit and never calls historical restart. */
export function abandonConversationOperation(cardId: string, recovery: Extract<RunRecovery, { action: 'abandon_session' }>): ApiOperation<{ carrier_id: string }> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/conversation/abandon`,
    body: recovery.request,
    headers: { 'Idempotency-Key': `abandon:${recovery.request.submission_id}:${recovery.request.revision}` },
    responseSchema: z.object({ card_id: z.string(), old_carrier_id: z.string(), carrier_id: z.string() }),
  };
}

/** Known failed END recovery is an explicit lifecycle action with its original CAS. */
export function recoverConversationOperation(cardId: string, recovery: Extract<RunRecovery, { action: 'recover_session' }>): ApiOperation<{ carrier_id: string }> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/conversation/recover`,
    body: recovery.request,
    headers: { 'Idempotency-Key': `recover:${recovery.request.carrier_id}:${recovery.request.attempt_id}:${recovery.request.revision}` },
    responseSchema: z.object({ card_id: z.string(), old_carrier_id: z.string(), carrier_id: z.string() }),
  };
}
