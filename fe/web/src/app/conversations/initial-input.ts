import { useQuery, useQueryClient } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { initialInputObservationOperation, observationMatchesReceipt } from '../../../../core/domain/conversation-initial-input.ts';
import { initialInputNeedsObservation, type AcceptedInitialInput, type ObservedInitialInput } from '../../../../core/domain/conversation-outbox.ts';
import { runOperation, queryKeys } from '../providers/queries.ts';
import { cancelThenInvalidate } from '../events/query-refresh.ts';

/** Correctness refresh for unresolved visible first inputs; never a latency or absence heuristic. */
const INITIAL_INPUT_RECONCILE_MS = 1000;

export function useInitialInputObservation(transport: ApiTransportPort, unauthorized: UnauthorizedChannel,
  cardId: string, op: AcceptedInitialInput | undefined, visible: boolean, sourcesFetching: boolean, nextRead: () => number) {
  const client = useQueryClient();
  const active = op !== undefined && initialInputNeedsObservation(op) ? op : undefined;
  return useQuery<ObservedInitialInput>({
    queryKey: [...queryKeys.initialInput(active?.trackId ?? '', cardId), active?.key ?? '', active?.receipt.entryId ?? ''],
    enabled: visible && active !== undefined,
    retry: false,
    // An unresolved receipt keeps its coherent fact across close/reopen; handoff releases its exact lease.
    gcTime: active === undefined ? 0 : Infinity,
    // Let the last observation's actual source reads finish before another periodic refresh replaces them.
    refetchInterval: query => visible && active !== undefined && query.state.error === null && !sourcesFetching ? INITIAL_INPUT_RECONCILE_MS : false,
    queryFn: async ({ signal }): Promise<ObservedInitialInput> => {
      if (active === undefined) throw new Error('No accepted first message to inspect.');
      const observation = await runOperation(transport,
        { ...initialInputObservationOperation(active.trackId, cardId, active.key), signal }, unauthorized);
      if (!observationMatchesReceipt(observation, active.receipt)) throw new Error('The first-message observation does not match its receipt.');
      // Unknown current presence cannot undo an earlier coherent fact about these original words.
      if (observation.kind === 'unavailable') throw new Error('The first message’s current status is unavailable.');
      // Every coherent boundary owns a fresh source pair, even when its payload repeats an older fact.
      // Publish immediately; only successful reads started after this boundary may settle its fence.
      const afterRead = nextRead();
      if (!signal.aborted) {
        void cancelThenInvalidate(client, ['harness-items', cardId]);
        void cancelThenInvalidate(client, ['planner-run', cardId]);
      }
      return { observation, afterRead };
    },
  });
}
