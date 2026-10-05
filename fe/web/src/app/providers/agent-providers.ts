// `GET /api/agent-providers` for the new-track model picker and Settings › Planners (#1817): one
// shared answer, and the Settings pane's Recheck that replaces it with a fresh one. A Recheck also
// re-fetches the Claude CLI's model list on the server (#1822), so every model catalog is refreshed.

import { useQueryClient } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  agentProvidersOperation, RECHECK_FAILURES, RECHECK_TEXT, type ProviderAvailability,
} from '../../../../core/domain/agent-providers.ts';
import { readWriteFailure } from '../../../../core/domain/failure-class.ts';
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

/** Recheck: one `refresh=true` read, written into the shared answer so the picker sees it too, and its catalogs refreshed. */
export function useAgentProvidersRecheck(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): AgentProvidersRecheck {
  const client = useQueryClient();
  const [state, setState] = useState<Readonly<{ rechecking: boolean; error: string | null }>>({ rechecking: false, error: null });
  const recheck = () => {
    if (state.rechecking) return;
    setState({ rechecking: true, error: null });
    runOperation(transport, agentProvidersOperation(true), unauthorized).then((answers) => {
      client.setQueryData(queryKeys.agentProviders(), answers);
      void client.invalidateQueries({ queryKey: queryKeys.modelCatalogPrefix() }).catch(() => undefined);
      setState({ rechecking: false, error: null });
    }, (failure: unknown) => {
      setState({ rechecking: false, error: readWriteFailure(failure, RECHECK_FAILURES, RECHECK_TEXT).text });
    });
  };
  return { recheck, rechecking: state.rechecking, error: state.error };
}
