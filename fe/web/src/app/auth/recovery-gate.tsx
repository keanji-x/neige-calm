import { QueryClientProvider, type QueryClient } from '@tanstack/react-query';
import { useEffect, type ReactNode } from 'react';
import { whoamiOperation } from '../../../../core/api/auth.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { RecoverySession } from '../../systems/recovery/session.ts';
import { observeRecoveryLifecycle, useRecoveryState } from '../../systems/recovery/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { WEB_COMPAT_VERSION, type ProviderRuntime, type ServerVersionInfo } from '../providers/public.tsx';
import { coordinateRecoveryQueries } from '../providers/recovery-queries.ts';
import { runOperation } from '../providers/queries.ts';
import { ReadReceiptScopeProvider } from '../providers/ui-preferences.tsx';
import { clearSessionArtifacts } from './session-gate.tsx';
import styles from './recovery-presentation.module.css';
import { RecoveryPresentation, RecoveryStatus } from './recovery-presentation.tsx';
export function RecoveryGate({ children, transport, unauthorized, client, runtime, cursorStore, renderLogin, renderEventBridge, recovery }: Readonly<{
  children: ReactNode; transport: ApiTransportPort; unauthorized: UnauthorizedChannel; client: QueryClient;
  runtime: ProviderRuntime; cursorStore: { clear(): void }; renderLogin: () => ReactNode;
  renderEventBridge?: (version: ServerVersionInfo) => ReactNode; recovery?: RecoverySession;
}>) {
  const [session] = useState(() => recovery ?? new RecoverySession({
    access: new RecoveryAccess(), storage: runtime.storage, origin: window.location.origin, compatibleVersion: WEB_COMPAT_VERSION,
    identity: (signal) => runOperation(transport, { ...whoamiOperation(), signal }, undefined),
    adoptScope: () => undefined,
    version: () => runtime.fetchVersion(), logout: () => Promise.resolve(),
    clear: () => clearSessionArtifacts(client, cursorStore, runtime), online: () => navigator.onLine, visible: () => !document.hidden,
  }));
  const state = useRecoveryState(session.access);
  useEffect(() => {
    const unsubscribe = unauthorized.subscribe(session.unauthorized);
    const releaseQueries = coordinateRecoveryQueries(session.access, client);
    const unobserve = observeRecoveryLifecycle(session);
    let disposed = false;
    let started = false;
    // StrictMode's discarded setup must not consume a one-document scan grant.
    if (session.scanOnly) queueMicrotask(() => { if (!disposed) { started = true; session.start(); } });
    else { started = true; session.start(); }
    return () => { disposed = true; unsubscribe(); unobserve(); if (started) session.stop(); releaseQueries(); };
  }, [client, session, unauthorized]);
  const hasScope = session.identity !== null && session.version !== null && transport.recovery !== undefined;
  const privateVisible = hasScope && state.phase !== 'login' && state.phase !== 'update';
  const eventsAllowed = privateVisible && ['connected', 'syncing'].includes(state.phase);
  return <QueryClientProvider client={client}><ThemeProvider storage={runtime.storage}>
    {privateVisible ? <div key={session.scopeRevision} className={styles.workspace}>
      <ReadReceiptScopeProvider id={session.version!.dbInstanceId}>{children}</ReadReceiptScopeProvider>
    </div>
      : state.phase === 'login' ? renderLogin() : state.phase === 'update' ? <main><h1>{state.detail}</h1><a href="http://tauri.localhost/">返回连接页</a></main>
        : <RecoveryPresentation />}
    {eventsAllowed && renderEventBridge?.(session.version!)}
    <RecoveryStatus state={state} retry={session.retry} />
  </ThemeProvider></QueryClientProvider>;
}
