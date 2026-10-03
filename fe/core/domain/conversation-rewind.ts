import { z } from 'zod';

import type { HarnessInputSegment, PlannerAttachment } from '../api/generated/wire.js';
import type { ApiOperation } from '../api/types.js';
import { harnessInputSegmentSchema, inputSegmentText } from './conversation.js';

/** What `POST …/planner/rewind` answers: the removed turn's input, prompt then accepted steers. */
export type PlannerRewind = Readonly<{
  card_id: string;
  turn_id: string;
  input: readonly HarnessInputSegment[];
}>;

/**
 * Remove the conversation's latest turn (#1923). Every refusal is a 409 with a readable reason and
 * nothing changed; `planner_harness_dormant` when no session is live.
 */
export function rewindPlannerTurnOperation(cardId: string, turnId: string): ApiOperation<PlannerRewind> {
  return {
    method: 'POST', path: `/api/cards/${encodeURIComponent(cardId)}/planner/rewind`,
    body: { turn_id: turnId },
    responseSchema: z.object({
      card_id: z.string(), turn_id: z.string(), input: z.array(harnessInputSegmentSchema),
    }),
  };
}

/** What goes back into the composer after an Edit. */
export type ComposerRefill = Readonly<{ text: string; attachments: readonly PlannerAttachment[] }>;

/** One message from the removed input: segment texts read as the transcript shows them, a blank line apart; the server lists each image once. */
export function composerRefillFrom(input: readonly HarnessInputSegment[]): ComposerRefill {
  return {
    text: input.map(inputSegmentText).filter((text) => text !== '').join('\n\n'),
    attachments: input.flatMap((segment) => segment.attachments),
  };
}
