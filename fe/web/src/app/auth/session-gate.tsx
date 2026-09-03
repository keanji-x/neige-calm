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
import type { SyncCursorPort } from '../../systems/events/cursor-port.ts';
import { useState } from '../../ui/state/public.ts';
import type { ProviderRuntime } from '../providers/public.tsx';

type SessionCleanupRuntime = Pick<ProviderRuntime, 'deleteDatabase' | 'idbDatabaseName'>;
type SessionCursorStore = Pick<SyncCursorPort, 'clear'>;

/** Clears every artifact whose ownership ends with the current session. */
export function clearSessionArtifacts(
  client: QueryClient,
  cursorStore: SessionCursorStore,
  runtime: SessionCleanupRuntime,
): void {
  try { client.clear(); } catch { /* cleanup is best-effort */ }
  try { cursorStore.clear(); } catch { /* storage may be unavailable */ }
  try { runtime.deleteDatabase(runtime.idbDatabaseName); } catch { /* indexedDB may be unavailable */ }
}

export function SessionGate({ children, transport, unauthorized, client, runtime, cursorStore, renderLogin, renderError }: Readonly<{
  children: ReactNode; transport: ApiTransportPort; unauthorized: UnauthorizedChannel; client: QueryClient;
  runtime: SessionCleanupRuntime; cursorStore: SessionCursorStore;
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
    // The probe's 401 is an expected session verdict, so it must not broadcast.
    void performApiRequest(transport, { ...whoamiOperation(), signal: controller.signal }, undefined).then((result) => {
      if (!mounted.current || epoch.current !== probeEpoch) return;
      const verdict = resolveSessionProbe(result);
      if (verdict.status === 'unauthed') clearSessionArtifacts(client, cursorStore, runtime);
      setState(verdict);
    });
    return () => { controller.abort(); if (activeProbe.current === controller) activeProbe.current = null; };
  }, [client, cursorStore, runtime, transport]);
  useEffect(() => {
    mounted.current = true;
    const cancel = probe();
    return () => { mounted.current = false; epoch.current += 1; cancel(); };
  }, [probe]);
  useEffect(() => unauthorized.subscribe(() => {
    epoch.current += 1;
    activeProbe.current?.abort();
    activeProbe.current = null;
    clearSessionArtifacts(client, cursorStore, runtime);
    setState({ status: 'unauthed' });
  }), [client, cursorStore, runtime, unauthorized]);
  if (state.status === 'unknown') return null;
  if (state.status === 'unauthed') return renderLogin();
  if (state.status === 'error') return renderError(probe);
  return children;
}
