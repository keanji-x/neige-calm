import { agentProviderSchema } from '../../../../core/api/schemas.ts';
import { PLANNER_CONVERSATION_KIND, type Conversation } from '../../../../core/domain/conversation.ts';
import { plannerCardIn, isTaskWorkerHarnessPayload } from '../../systems/cards/public.ts';
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

/** Assistants belong to the server list; detail supplies the current Planner and read-only task workers.
 * A stale list row for that card is replaced, so query arrival order cannot duplicate roles.
 */
export function trackConversationRows(trackId: string, trackTitle: string,
  cards: TrackDetailWire['cards'], listed: readonly Conversation[]): readonly Conversation[] {
  const rows = new Map<string, Conversation>(listed.filter(row => row.trackId === trackId).map(row => [row.id, { ...row, trackTitle }]));
  for (const card of cards) {
    if (!(card.kind === 'codex' && isTaskWorkerHarnessPayload(card.payload)) && card.runtime?.kind !== 'managed-worker') continue;
    const payload = card.payload as { worker_provider?: unknown } | null;
    const provider = agentProviderSchema.safeParse(card.runtime?.provider ?? payload?.worker_provider);
    if (!provider.success) continue;
    rows.set(card.id, { id: card.id, trackId, trackTitle, title: card.title ?? 'Task worker', kind: 'codex',
      provider: provider.data, readOnly: true,
      state: card.runtime?.status ?? null, updatedAt: card.runtime?.updated_at_ms ?? card.updated_at,
      lastTurnCompletedAt: card.runtime?.last_turn_completed_ms ?? null });
  }
  const planner = plannerConversationRow(trackId, trackTitle, plannerCardIn(cards));
  if (planner !== null) rows.set(planner.id, planner);
  return [...rows.values()].toSorted((a, b) => Number(b.kind === PLANNER_CONVERSATION_KIND) - Number(a.kind === PLANNER_CONVERSATION_KIND));
}
