/**
 * SessionGate is outside ServerCompatGate. The whoami verdict therefore lands
 * before either the router or `/api/version` mounts: a logged-out deep link
 * cannot let a route loader (or the compatibility query) leak a preliminary
 * 401. Once authenticated, ServerCompatGate remains the next inner gate.
 */
import { type QueryClient } from '@tanstack/react-query';
import { useCallback, useEffect, useRef, type ReactNode } from 'react';
import { whoamiOperation } from '../../../../core/api/auth.ts';
import { performApiRequest } from '../../../../core/api/client.ts';
import { resolveSessionProbe, type SessionProbeState } from '../../../../core/api/session.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { useState } from '../../ui/state/public.ts';

export function SessionGate({ children, transport, unauthorized, client, renderLogin, renderError }: Readonly<{
  children: ReactNode; transport: ApiTransportPort; unauthorized: UnauthorizedChannel; client: QueryClient;
  renderLogin: () => ReactNode; renderError: (retry: () => void) => ReactNode;
}>) {
  const [state, setState] = useState<SessionProbeState<unknown>>({ status: 'unknown' });
  const epoch = useRef(0);
  const mounted = useRef(false);
  const probe = useCallback(() => {
    const probeEpoch = ++epoch.current;
    setState({ status: 'unknown' });
    void performApiRequest(transport, whoamiOperation()).then((result) => {
      if (mounted.current && epoch.current === probeEpoch) setState(resolveSessionProbe(result));
    });
  }, [transport]);
  useEffect(() => {
    mounted.current = true;
    probe();
    return () => { mounted.current = false; epoch.current += 1; };
  }, [probe]);
  useEffect(() => unauthorized.subscribe(() => {
    epoch.current += 1;
    client.clear();
    setState({ status: 'unauthed' });
  }), [client, unauthorized]);
  if (state.status === 'unknown') return null;
  if (state.status === 'unauthed') return renderLogin();
  if (state.status === 'error') return renderError(probe);
  return children;
}
