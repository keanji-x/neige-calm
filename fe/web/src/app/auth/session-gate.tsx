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
  const activeProbe = useRef<AbortController | null>(null);
  const probe = useCallback(() => {
    activeProbe.current?.abort();
    const controller = new AbortController();
    activeProbe.current = controller;
    const probeEpoch = ++epoch.current;
    setState({ status: 'unknown' });
    void performApiRequest(transport, { ...whoamiOperation(), signal: controller.signal }).then((result) => {
      if (mounted.current && epoch.current === probeEpoch) setState(resolveSessionProbe(result));
    });
    return () => { controller.abort(); if (activeProbe.current === controller) activeProbe.current = null; };
  }, [transport]);
  useEffect(() => {
    mounted.current = true;
    const cancel = probe();
    return () => { mounted.current = false; epoch.current += 1; cancel(); };
  }, [probe]);
  useEffect(() => unauthorized.subscribe(() => {
    epoch.current += 1;
    activeProbe.current?.abort();
    activeProbe.current = null;
    client.clear();
    setState({ status: 'unauthed' });
  }), [client, unauthorized]);
  if (state.status === 'unknown') return null;
  if (state.status === 'unauthed') return renderLogin();
  if (state.status === 'error') return renderError(probe);
  return children;
}
