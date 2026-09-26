// `GET /api/agent-providers` for the new-track model picker and Settings › Planners (#1817): one
// shared answer, and the Settings pane's Recheck that replaces it with a fresh one.

import { useQueryClient } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { agentProvidersOperation, type ProviderAvailability } from '../../../../core/domain/agent-providers.ts';
import { useState } from '../../ui/state/public.ts';
import { queryKeys, runOperation } from './queries.ts';

export function agentProvidersQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.agentProviders(),
    queryFn: ({ signal }: { signal: AbortSignal }): Promise<ProviderAvailability[]> =>
      runOperation(transport, { ...agentProvidersOperation(false), signal }, unauthorized),
  };
}

export type AgentProvidersRecheck = Readonly<{
  recheck: () => void;
  rechecking: boolean;
  /** Why the last recheck failed; `null` once one succeeds or while none has failed. */
  error: string | null;
}>;

/** Recheck: one `refresh=true` read, written into the shared answer so the picker sees it too. */
export function useAgentProvidersRecheck(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): AgentProvidersRecheck {
  const client = useQueryClient();
  const [state, setState] = useState<Readonly<{ rechecking: boolean; error: string | null }>>({ rechecking: false, error: null });
  const recheck = () => {
    if (state.rechecking) return;
    setState({ rechecking: true, error: null });
    runOperation(transport, agentProvidersOperation(true), unauthorized).then((answers) => {
      client.setQueryData(queryKeys.agentProviders(), answers);
      setState({ rechecking: false, error: null });
    }, (failure: unknown) => {
      setState({ rechecking: false, error: failure instanceof Error ? failure.message : 'The recheck failed.' });
    });
  };
  return { recheck, rechecking: state.rechecking, error: state.error };
}
