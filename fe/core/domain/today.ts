// The Today launchpad resolve: the kernel's `purpose = 'launchpad'` track, plus the one fact the
// track detail cannot answer — whether its report holds anything but the freshly-minted skeleton.
// That is the server's predicate; nothing here parses the report body. The endpoint is a pure
// read and does NOT bootstrap.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';
import type { FailureTable, WriteClass, WriteText } from './failure-class.js';
import { trackConversationCardId, type Conversation } from './conversation.js';

export const todayLaunchpadSchema = z.object({
  track_id: z.string(),
  /** Whether the report's CURRENT content differs from a freshly-minted one; a revert un-flips it. */
  report_has_noninitial_content: z.boolean(),
});

export type TodayLaunchpadWire = z.infer<typeof todayLaunchpadSchema>;

/** The Today page load's only new request. `null` is data, not a failure: a fresh workspace has no launchpad track. */
export function todayLaunchpadOperation(): ApiOperation<TodayLaunchpadWire | null> {
  return {
    method: 'GET',
    path: '/api/today/launchpad',
    responseSchema: todayLaunchpadSchema.nullable(),
  };
}

export const todayLaunchpadEnsureSchema = z.object({ track_id: z.string() });

export type TodayLaunchpadEnsureWire = z.infer<typeof todayLaunchpadEnsureSchema>;

/** Materialise the Today launchpad after an explicit user action; may wait on the agent service. */
export function todayLaunchpadEnsureOperation(): ApiOperation<TodayLaunchpadEnsureWire> {
  return {
    method: 'POST',
    path: '/api/today/launchpad/ensure',
    responseSchema: todayLaunchpadEnsureSchema,
  };
}

/**
 * What a failed ensure says. It is get-or-create and starts the assistant under a fixed key, so a retry is safe; only a
 * 403 refuses it. Every other answer, a 500 from the assistant's start included, leaves the start unknown.
 */
export const LAUNCHPAD_ENSURE_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([Object.freeze({ status: Object.freeze([403]), is: 'refused' as const })]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const LAUNCHPAD_ENSURE_TEXT: WriteText = Object.freeze({
  refused: 'Today assistant was not started.', unknown: 'Starting Today assistant is unconfirmed.',
});

export const todayReportResetSchema = z.object({
  /** The launchpad track whose report was restored. */
  track_id: z.string(),
  /** What `GET /api/today/launchpad` will now report — always `false`; returned so the caller sees the reset land. */
  report_has_noninitial_content: z.boolean(),
});

export type TodayReportResetWire = z.infer<typeof todayReportResetSchema>;

/** The server's fixed idempotency key for the launchpad's summary writer. */
export const TODAY_SUMMARY_CONVERSATION_KEY = 'today-summary';
export const TODAY_SUMMARY_CONVERSATION_TITLE = 'Today’s progress';

/**
 * The summary writer's first persisted user turn is an internal bootstrap instruction, so the usual
 * title fallback would expose it.
 */
export function nameTodaySummaryConversation(trackId: string, row: Conversation): Conversation {
  if (row.title !== null || row.id !== trackConversationCardId(trackId, TODAY_SUMMARY_CONVERSATION_KEY)) {
    return row;
  }
  return { ...row, title: TODAY_SUMMARY_CONVERSATION_TITLE };
}

/**
 * What a failed reset says. The server reads the current revision itself, so a retry is safe: 403 and 404 refuse it,
 * and so would a 400, which writes nothing either. A 409 is a revision race with a writer, and it and anything else
 * leave it unknown.
 */
export const REPORT_RESET_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([Object.freeze({ status: Object.freeze([400, 403, 404]), is: 'refused' as const })]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const REPORT_RESET_TEXT: WriteText = Object.freeze({
  refused: 'Today’s report was not reset.', unknown: 'The report reset is unconfirmed.',
});

/**
 * Put today's report back to its canonical empty state. It sends no document, and there must
 * never be a parameter for one: the canonical document is kernel-owned. Destructive; touches
 * the report only.
 */
export function todayReportResetOperation(): ApiOperation<TodayReportResetWire> {
  return {
    method: 'POST',
    path: '/api/today/launchpad/report/reset',
    responseSchema: todayReportResetSchema,
  };
}
