import { z } from 'zod';

import type { AgentProvider, PlannerConversation, PlannerConversationStarted } from '../api/generated/wire.js';
import type { ApiFailure, ApiOperation } from '../api/types.js';
import type { Conversation } from './conversation.js';
import { refusalText, type FailureTable, type NotSentError, type WriteFailure, type WriteText } from './failure-class.js';

/**
 * A new, blank Planner conversation on the same card (#2774). The card stays the Planner; each earlier conversation is
 * read-only history, read through `harness/items?conversation=<ordinal>` and never written.
 */
export const PLANNER_HISTORY_KIND = 'planner-history' as const;

/** One earlier conversation's row key: the card and the ordinal the server lists it under. */
export function plannerHistoryKey(cardId: string, ordinal: number): string {
  return `${cardId}#${ordinal}`;
}

/** The card and ordinal a history key names, or `null` for any other id. A card id never carries `#`. */
export function parsePlannerHistoryKey(id: string): Readonly<{ cardId: string; ordinal: number }> | null {
  const match = /^([^#]+)#(0|[1-9]\d*)$/.exec(id);
  return match === null ? null : { cardId: match[1], ordinal: Number(match[2]) };
}

const plannerConversationSchema: z.ZodType<PlannerConversation> = z.object({
  ordinal: z.number().int().nonnegative(), started_at_ms: z.number(), ended_at_ms: z.number(),
});

/** `GET …/planner/conversations`: the earlier conversations that still hold transcript rows, oldest first. */
export function plannerConversationsOperation(cardId: string): ApiOperation<PlannerConversation[]> {
  return {
    method: 'GET', path: `/api/cards/${encodeURIComponent(cardId)}/planner/conversations`,
    responseSchema: z.array(plannerConversationSchema),
  };
}

/**
 * `POST …/planner/conversations`. The key identifies one confirmed press: a replay under it answers the first request's
 * body and starts nothing, so a new confirmation mints a new key.
 */
export function startPlannerConversationOperation(cardId: string, idempotencyKey: string): ApiOperation<PlannerConversationStarted> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/planner/conversations`,
    headers: { 'Idempotency-Key': idempotencyKey },
    responseSchema: z.object({ card_id: z.string(), conversation: z.number(), new_thread_id: z.string() }),
  };
}

/**
 * `refused`: the documented answers given before anything changes (400 a bad key, 403 not the person or not a Planner,
 * 404 no such card, 409 busy, unconfirmed, closed or changed, 503 still settling). A 500 is a start that failed after
 * the cutover, and a lost answer may have cut over: both are `unknown`, and the reads after it say which.
 */
export const PLANNER_NEW_CONVERSATION_FAILURES: FailureTable<WriteFailure> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([400, 403, 404, 409, 503]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const PLANNER_NEW_CONVERSATION_TEXT: WriteText = Object.freeze({
  refused: 'The new conversation was not started.',
  unknown: 'Starting a new conversation is unconfirmed.',
});

/** The one line a failed start says: the server's reason for a refusal, else the fixed unconfirmed one. */
export function plannerNewConversationFailureText(failure: ApiFailure | NotSentError | null): string {
  return refusalText(failure, PLANNER_NEW_CONVERSATION_FAILURES, PLANNER_NEW_CONVERSATION_TEXT.refused)
    ?? PLANNER_NEW_CONVERSATION_TEXT.unknown;
}

export const PLANNER_NEW_CONVERSATION_COPY = Object.freeze({
  action: 'New conversation',
  title: 'Start a new Planner conversation?',
  description: 'This conversation becomes read-only history. The Report, tasks and running work continue.',
  confirm: 'Start new conversation',
  cancel: 'Cancel',
});

export const PLANNER_HISTORY_NOTICE = 'This is an earlier Planner conversation. It is read-only.';

function pad(value: number): string {
  return String(value).padStart(2, '0');
}

/** Local `YYYY-MM-DD HH:mm`: two conversations started on one day still read apart. */
function startedLabel(atMs: number): string {
  const at = new Date(atMs);
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())} ${pad(at.getHours())}:${pad(at.getMinutes())}`;
}

/**
 * The Planner card's earlier conversations as archived, read-only rows. `archivedAt` and `updatedAt` are when the next
 * conversation started, so they sort and group with the other archived rows; there is no completion to read as unread.
 */
export function plannerHistoryRows(planner: Readonly<{ cardId: string; trackId: string; trackTitle: string; provider: AgentProvider }>,
  conversations: readonly PlannerConversation[]): Conversation[] {
  return conversations.map((conversation) => ({
    id: plannerHistoryKey(planner.cardId, conversation.ordinal),
    trackId: planner.trackId, trackTitle: planner.trackTitle, provider: planner.provider,
    title: `Earlier Planner conversation · ${startedLabel(conversation.started_at_ms)}`,
    kind: PLANNER_HISTORY_KIND, state: null,
    archivedAt: conversation.ended_at_ms, updatedAt: conversation.ended_at_ms, lastTurnCompletedAt: null,
  }));
}
