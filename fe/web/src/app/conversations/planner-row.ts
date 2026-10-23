import { PLANNER_CONVERSATION_KIND, type Conversation } from '../../../../core/domain/conversation.ts';
import { isAssistantHarnessPayload, isPlannerHarnessPayload } from '../../systems/cards/public.ts';
import { plannerProviderOf, type TrackDetailWire } from '../../../../core/domain/track.ts';

/** One projection shared by the open conversation and workspace history. */
export function plannerConversationRow(trackId: string, trackTitle: string,
  planner: TrackDetailWire['cards'][number] | undefined): Conversation | null {
  return planner === undefined ? null : {
    provider: plannerProviderOf(planner.payload),
    id: planner.id, trackId, trackTitle, title: planner.title, kind: PLANNER_CONVERSATION_KIND,
    state: planner.runtime?.status ?? null,
    updatedAt: planner.runtime?.updated_at_ms ?? planner.updated_at,
    lastTurnCompletedAt: planner.runtime?.last_turn_completed_ms ?? null,
  };
}

/** Detail supplies the complete role pair in one snapshot; the list augments rows and metadata.
 * In particular, a list from either side of a role swap cannot remove either card.
 */
export function trackConversationRows(trackId: string, trackTitle: string,
  cards: TrackDetailWire['cards'], listed: readonly Conversation[]): readonly Conversation[] {
  const metadata = new Map(listed.map(row => [row.id, row]));
  const rows = new Map<string, Conversation>(listed.filter(row => row.trackId === trackId).map(row => [row.id, { ...row, trackTitle }]));
  for (const card of cards) {
    if (card.kind !== 'codex') continue;
    const planner = plannerConversationRow(trackId, trackTitle, card);
    if (planner === null) continue;
    if (isAssistantHarnessPayload(card.payload)) rows.set(card.id, {
      ...planner, ...metadata.get(card.id), id: card.id, trackId, trackTitle, kind: 'track-assistant',
    } satisfies Conversation);
    // Use the same owner predicate as the Planner entry, not every non-Assistant card.
    else if (isPlannerHarnessPayload(card.payload)) rows.set(card.id, planner);
  }
  return [...rows.values()];
}
