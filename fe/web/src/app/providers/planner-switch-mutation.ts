import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { plannerSwitchOperation, type PlannerSwitchRequest } from '../../../../core/domain/planner-switch.ts';
import { runOperation } from './queries.ts';
import { useRecoveryMutation } from './recovery-mutation.ts';

/** Recovery admission and retry:false are shared with the other human writes. */
export function usePlannerSwitchMutation(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return useRecoveryMutation(transport, {
    mutationFn: ({ trackId, body }: { trackId: string; body: PlannerSwitchRequest }, admitted) =>
      runOperation(admitted, plannerSwitchOperation(trackId, body), unauthorized),
  });
}
