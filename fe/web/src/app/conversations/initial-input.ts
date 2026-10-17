import { useQuery, useQueryClient } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { initialInputObservationOperation, observationMatchesReceipt } from '../../../../core/domain/conversation-initial-input.ts';
import type { AcceptedInitialInput, ObservedInitialInput } from '../../../../core/domain/conversation-outbox.ts';
import { runOperation, queryKeys } from '../providers/queries.ts';
import { cancelThenInvalidate } from '../events/query-refresh.ts';

/** Correctness refresh for unresolved visible first inputs; never a latency or absence heuristic. */
const INITIAL_INPUT_RECONCILE_MS = 1000;

export function useInitialInputObservation(transport: ApiTransportPort, unauthorized: UnauthorizedChannel,
  cardId: string, op: AcceptedInitialInput | undefined, visible: boolean, sourcesFetching: boolean, nextRead: () => number) {
  const client = useQueryClient();
  return useQuery({
    queryKey: [...queryKeys.initialInput(op?.trackId ?? '', cardId), op?.key ?? '', op?.receipt.entryId ?? ''],
    enabled: visible && op !== undefined,
    retry: false,
    // The receipt's last coherent fact remains owned across close/reopen until its outbox retirement removes it.
    gcTime: op === undefined ? 0 : Infinity,
    // Let the last observation's actual source reads finish before another periodic refresh replaces them.
    refetchInterval: visible && op !== undefined && !sourcesFetching ? INITIAL_INPUT_RECONCILE_MS : false,
    queryFn: async ({ signal }): Promise<ObservedInitialInput> => {
      if (op === undefined) throw new Error('No accepted first message to inspect.');
      const observation = await runOperation(transport,
        { ...initialInputObservationOperation(op.trackId, cardId, op.key), signal }, unauthorized);
      if (!observationMatchesReceipt(observation, op.receipt)) throw new Error('The first-message observation does not match its receipt.');
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
