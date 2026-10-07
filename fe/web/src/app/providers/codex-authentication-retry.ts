// Only an explicit owner intent may rearm the provider's durable observation revision.
import { useRef } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { codexAuthenticationRetryOperation, authenticationRetryFailureText, type AuthenticationRetryResponse } from '../../../../core/domain/agent-providers.ts';
import { useState } from '../../ui/state/public.ts';
import { useRecoveryMutation } from './recovery-mutation.ts';
import { queryKeys, runOperation } from './queries.ts';

export function useCodexAuthenticationRetry(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  const client = useQueryClient();
  const running = useRef(false);
  const [state, setState] = useState<Readonly<{ error: string | null; notices: AuthenticationRetryResponse['recovery_notices'] }>>({ error: null, notices: [] });
  const mutation = useRecoveryMutation(transport, {
    gcTime: 0,
    mutationFn: (revision: string, admitted: ApiTransportPort) => runOperation(admitted, codexAuthenticationRetryOperation(revision), unauthorized),
    onMutate: () => { setState({ error: null, notices: [] }); },
    onSuccess: (answer) => { setState({ error: null, notices: answer.recovery_notices }); },
    onError: (error) => { setState({ error: authenticationRetryFailureText(error), notices: [] }); },
    onSettled: () => { void client.invalidateQueries({ queryKey: queryKeys.agentProviders() }); },
  });
  return {
    pending: mutation.isPending,
    error: state.error,
    notices: state.notices,
    retry: (revision: string) => {
      if (running.current) return;
      running.current = true;
      void mutation.mutateAsync(revision).catch(() => undefined).finally(() => { running.current = false; mutation.reset(); });
    },
  };
}
