import type { Conversation } from '../../../../core/domain/conversation.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';

export const PLANNER_CONVERSATION_KIND = 'shared-spec' as const;

/** One projection shared by the open conversation and workspace history. */
export function plannerConversationRow(trackId: string, trackTitle: string,
  planner: TrackDetailWire['cards'][number] | undefined): Conversation | null {
  return planner === undefined ? null : {
    id: planner.id, trackId, trackTitle, title: planner.title, kind: PLANNER_CONVERSATION_KIND,
    state: planner.runtime?.status ?? null,
    updatedAt: planner.runtime?.updated_at_ms ?? planner.updated_at,
    lastTurnCompletedAt: planner.runtime?.last_turn_completed_ms ?? null,
  };
}
