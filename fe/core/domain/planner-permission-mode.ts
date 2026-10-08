// A Planner card's permission mode (#2348, #2441): whether its provider stays in its sandbox, may
// pause a turn to ask the person before it acts outside it, or runs with no sandbox at all. `GET /api/cards/{id}/planner/run` reads it; the one write is
// `PUT /api/cards/{id}/planner/permission-mode`, which only the person may make. A change applies
// from the Planner's next turn.

import { z } from 'zod';
import type { PlannerPermissionMode, SetPlannerPermissionModeResponse } from '../api/generated/wire.js';
import type { ApiOperation } from '../api/types.js';
import type { FailureTable, WriteFailure, WriteText } from './failure-class.js';

/** The closed set the server stores; `null` on the run read means the card is not a Planner. */
export const plannerPermissionModeSchema: z.ZodType<PlannerPermissionMode> = z.enum(['never', 'ask', 'full']);

/** Store `mode` on the card. The answer echoes the stored mode rather than the one asked for. */
export function setPlannerPermissionModeOperation(
  cardId: string, mode: PlannerPermissionMode,
): ApiOperation<SetPlannerPermissionModeResponse> {
  return {
    method: 'PUT',
    path: `/api/cards/${encodeURIComponent(cardId)}/planner/permission-mode`,
    body: { permission_mode: mode },
    responseSchema: z.object({ card_id: z.string(), permission_mode: plannerPermissionModeSchema }),
  };
}

/**
 * What a failed mode change says. 403 (not the person, or not a Planner card), 404 and 422 are
 * answered before anything is stored; a 5xx, a lost or an unreadable answer may follow a stored one.
 */
export const PLANNER_PERMISSION_MODE_FAILURES: FailureTable<WriteFailure> = Object.freeze({
  rules: Object.freeze([Object.freeze({ status: Object.freeze([403, 404, 422]), is: 'refused' as const })]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const PERMISSION_MODE_CHANGE_TEXT: WriteText = Object.freeze({
  refused: 'The approval setting was not changed.', unknown: 'The approval setting change is unconfirmed.',
});
