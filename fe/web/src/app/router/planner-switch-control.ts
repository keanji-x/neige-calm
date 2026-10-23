import { useRef } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';
import { ApiError } from '../../../../core/domain/failure-class.ts';
import { plannerSwitchFailure, PLANNER_SWITCH_UNCONFIRMED } from '../../../../core/domain/planner-switch.ts';
import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import { isAssistantHarnessPayload, plannerCardIn } from '../../systems/cards/public.ts';
import { useCommittedCallback } from '../../ui/state/committed-callback.ts';
import { usePlannerSwitchMutation } from '../providers/planner-switch-mutation.ts';
import { trackDetailQueryOptions } from '../providers/queries.ts';
import { cancelThenInvalidate } from '../events/query-refresh.ts';
import type { PlannerConversationScope } from '../conversations/contracts.ts';

/** Owns one pane's human intent; scope is the selected conversation, never the page Track. */
export function usePlannerSwitchControl(transport: ApiTransportPort, unauthorized: UnauthorizedChannel, scope: PlannerConversationScope | null) {
  const client = useQueryClient();
  const mutation = usePlannerSwitchMutation(transport, unauthorized);
  const pending = useRef(false);
  return useCommittedCallback(scope === null ? null : `${scope.id}:${scope.cardId}`, async () => {
    if (scope === null || pending.current) return false;
    pending.current = true;
    const options = trackDetailQueryOptions(transport, scope.id, unauthorized);
    let detail: TrackDetailWire | undefined;
    try {
      const snapshot: TrackDetailWire = client.getQueryData<TrackDetailWire>(options.queryKey)
        ?? await client.fetchQuery({ ...options, retry: false });
      detail = snapshot;
      const planner = plannerCardIn(snapshot.cards);
      if (planner === undefined) throw new Error(PLANNER_SWITCH_UNCONFIRMED.en);
      const refresh = async () => {
        // Same authoritative CardUpdated plan as the stream; cancel stale reads before replacement.
        const keys = snapshot.cards.filter(card => card.id === planner.id || card.id === scope.cardId).flatMap(card => invalidationPlanFor({
          ev: 'card.updated', data: { ...card, title: card.title ?? undefined },
        }).invalidate);
        await Promise.all([...new Map(keys.map(key => [JSON.stringify(key), key])).values()]
          .map(key => cancelThenInvalidate(client, key)));
        return client.fetchQuery({ ...options, retry: false }).catch(() => null);
      };
      let receipt;
      try {
        receipt = await mutation.mutateAsync({ trackId: scope.id, body: {
          target_assistant_card_id: scope.cardId, expected_planner_card_id: planner.id,
          expected_revision: snapshot.planner_revision,
        } });
      } catch (error) {
        await refresh();
        const failure = plannerSwitchFailure(error instanceof ApiError ? error.failure : null);
        const card = snapshot.cards.find(card => card.id === failure.cardId);
        const role = failure.cardId === planner.id ? 'Original Planner' : failure.cardId === scope.cardId ? 'Target Assistant' : null;
        throw new Error(`${failure.message}${role === null ? '' : ` (${role}: ${card?.title ?? failure.cardId})`}`);
      }
      const confirmed = await refresh();
      if (confirmed === null || receipt.track_id !== scope.id || receipt.planner_card_id !== scope.cardId || receipt.demoted_card_id !== planner.id
        || confirmed.planner_revision < receipt.planner_revision || plannerCardIn(confirmed.cards)?.id !== scope.cardId
        || !confirmed.cards.some(card => card.id === planner.id && isAssistantHarnessPayload(card.payload))) {
        throw new Error(PLANNER_SWITCH_UNCONFIRMED.en);
      }
      return true;
    } catch (error) {
      if (detail === undefined) throw new Error(PLANNER_SWITCH_UNCONFIRMED.en);
      throw error;
    } finally { pending.current = false; }
  });
}
