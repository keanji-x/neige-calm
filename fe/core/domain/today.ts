// The Today launchpad resolve: the kernel's `purpose = 'launchpad'` track, plus the one fact the
// track detail cannot answer — whether its report holds anything but the freshly-minted skeleton.
// That is the server's predicate; nothing here parses the report body. The endpoint is a pure
// read and does NOT bootstrap.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';
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
