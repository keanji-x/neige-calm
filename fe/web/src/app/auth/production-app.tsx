import { parseShareBasePath, shareAccessOperation } from '../../../../core/domain/share.ts';
import type { ShareScope } from '../../../../core/api/generated/wire.ts';
import { ApiError } from '../../../../core/domain/failure-class.ts';
import { ShareUnlockGate, takeShareTokenFromHash } from './share-unlock.tsx';
import { createShareStorage } from './share-storage.ts';
import { useState } from '../../ui/state/public.ts';
import { useEffect } from 'react';
import { DepthFeedback } from '../../ui/motion/depth-feedback.tsx';
import { runWorkerTask } from '../../systems/worker-task/public.ts';
import type { PreparedReport } from '../../features/report/document/preparation.ts';
import type { TrackReport } from '../../../../core/domain/report.ts';
import { useMemo } from 'react';
import { GitHubPreviewProvider } from '../../systems/github-links/public.tsx';
import { githubPreviewOperation, type GitHubPreviewPort } from '../../../../core/domain/github-preview.ts';
import { QueryClient } from '@tanstack/react-query';
import { RouterProvider, type AnyRouter } from '@tanstack/react-router';
import { StrictMode, type ReactNode } from 'react';
import { RecoveryGate } from './recovery-gate.tsx';
import { RecoverySession } from '../../systems/recovery/session.ts';
import { createScanPairingPort, takeScanInput } from '../../systems/recovery/scan.ts';
import { createRecoveryUnauthorizedChannel } from '../../systems/recovery/unauthorized.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { whoamiOperation } from '../../../../core/api/auth.ts';
import { createRoot } from 'react-dom/client';
import { createUnauthorizedChannel, type UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { IDB_DB_NAME } from '../../../../core/keys/storage.ts';
import { LoginPage } from '../../features/auth/login-page/public.tsx';
import { loginForRecovery, loginWithTransport } from './login.ts';
import { clearSessionArtifacts, SessionGate } from './session-gate.tsx';
import { createBrowserEventComposition } from '../composition.ts';
import { EventBridge } from '../events/event-bridge.tsx';
import { RecoveryEventBridge } from '../events/recovery-event-bridge.tsx';
import { createBrowserCursorStore } from '../events/browser-cursor-store.ts';
import { AppProviders, WEB_COMPAT_VERSION, type ProviderRuntime } from '../providers/public.tsx';
import { logoutOperation, runOperation, serverVersionOperation } from '../providers/queries.ts';
import { createFetchTransport } from '../providers/transport.ts';
import { createCardFilesPort } from '../providers/directory.ts';
import { createClaudeCardPort } from '../providers/claude-card.ts';
import { createAppRouter } from '../router/public.tsx';
import { createUiPreferences } from '../providers/ui-preferences.tsx';
import { createRecentFileHistory } from '../providers/recent-files.ts';
import { createCardHost, createCardRegistry } from '../../systems/cards/public.js';
import { bootCards } from '../cards.ts';
import { BundledConnectionNotice, BundledLoginPage } from './bundled-connection.tsx';

export function ProductionApp({ transport, unauthorized, client, runtime, cursorStore, router, renderEventBridge,
  renderLogin, renderError, recovery, shareLinkId }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; client: QueryClient; runtime: ProviderRuntime;
  cursorStore: Parameters<typeof AppProviders>[0]['cursorStore']; router: AnyRouter | ((scope: ShareScope) => AnyRouter);
  shareLinkId?: string;
  renderEventBridge?: Parameters<typeof AppProviders>[0]['renderEventBridge'];
  recovery?: RecoverySession;
  renderLogin: (retry: () => void) => ReactNode; renderError: (retry: () => void) => ReactNode;
}>) {
  const previewPort = useMemo<GitHubPreviewPort>(() => ({
    read: (target, signal) => runOperation(transport, githubPreviewOperation(target, signal), unauthorized),
  }), [transport, unauthorized]);
  const content = <GitHubPreviewProvider port={previewPort}><AuthenticatedRouter router={router} shareLinkId={shareLinkId} unauthorized={unauthorized} /></GitHubPreviewProvider>;
  if (__NC_BUNDLED__) return <StrictMode><DepthFeedback /><RecoveryGate transport={transport} unauthorized={unauthorized} client={client}
    runtime={runtime} cursorStore={cursorStore} renderLogin={() => renderLogin(() => {})} renderEventBridge={renderEventBridge} recovery={recovery}>
    {content}
  </RecoveryGate></StrictMode>;
  return <StrictMode><DepthFeedback /><SessionGate transport={transport} unauthorized={unauthorized} client={client}
    runtime={runtime} cursorStore={cursorStore}
    renderLogin={renderLogin} renderError={renderError}>
    <AppProviders client={client} runtime={runtime} cursorStore={cursorStore} renderEventBridge={renderEventBridge}>
      {content}
    </AppProviders>
  </SessionGate></StrictMode>;
}

export function mountProductionApp(root: HTMLElement, browserInput: Readonly<{
  storage: Storage; submittedStorage?: () => Storage; reload: () => void; deleteDatabase: (name: string) => void;
}>): void {
  const prepareReport = (report: TrackReport | null, signal: AbortSignal) => runWorkerTask<TrackReport | null, PreparedReport>(
    () => new Worker(new URL('../../features/report/document/preparation.worker.ts', import.meta.url), { type: 'module' }), report, signal);
  const shareRoute = parseShareBasePath(window.location.pathname);
  if (shareRoute === null && /^\/next\/share(?:\/|$)/.test(window.location.pathname)) {
    const mounted = createRoot(root);
    mounted.render(<main><p role="alert">分享链接不存在或已过期。</p></main>);
    return;
  }
  const shareLinkId = shareRoute?.shareId;
  const initialShareToken = shareLinkId === undefined ? undefined : takeShareTokenFromHash();
  const browser = shareLinkId === undefined ? browserInput : {
    ...browserInput,
    storage: createShareStorage(browserInput.storage, shareLinkId),
    submittedStorage: browserInput.submittedStorage === undefined ? undefined
      : () => createShareStorage(browserInput.submittedStorage!(), shareLinkId),
  };
  const storage = browser.storage;
  const access = new RecoveryAccess();
  const scheduler = { enqueue: (task: () => void) => queueMicrotask(task) };
  const unauthorized = __NC_BUNDLED__ ? createRecoveryUnauthorizedChannel(access, scheduler)
    : createUnauthorizedChannel(scheduler, { report: console.error });
  const unlockTransport = createFetchTransport();
  const base = createFetchTransport({ shareLinkId });
  const guarded = createRecoveryTransports(base, access);
  const transport = __NC_BUNDLED__ ? guarded.business : base;
  const probe = __NC_BUNDLED__ ? guarded.probe : base;
  const client = new QueryClient();
  const events = __NC_BUNDLED__ ? null
    : createBrowserEventComposition({ storage, transport: probe, unauthorizedChannel: unauthorized,
      url: shareLinkId === undefined ? undefined : `${window.location.protocol === 'https:' ? 'wss:' : 'ws:'}//${window.location.host}/api/events?share_link=${encodeURIComponent(shareLinkId)}` });
  const cursorStore = events?.store ?? createBrowserCursorStore(storage);
  const createRecoveryEvents = () => createBrowserEventComposition({ storage, transport: probe,
    cursorStore, unauthorizedChannel: unauthorized,
    probeUnauthorized: () => { recovery?.resume(); return Promise.resolve(); } });
  // The one place the card runtime is assembled: no module-level registry, so a second boot would be a second registry.
  const registry = createCardRegistry();
  bootCards(registry);
  // Built from this app's transport and 401 channel so a card's read hits the same session handling as every other read.
  const host = createCardHost(registry, {
    files: createCardFilesPort(transport, unauthorized, shareLinkId),
    claude: createClaudeCardPort(transport, unauthorized, client),
    ...(__NC_BUNDLED__ ? { recovery: access } : {}),
  });
  const runtime: ProviderRuntime = {
    fetchVersion: () => runOperation(probe, serverVersionOperation(), unauthorized),
    reload: browser.reload, deleteDatabase: browser.deleteDatabase,
    idbDatabaseName: shareLinkId === undefined ? IDB_DB_NAME : `${IDB_DB_NAME}-share-${shareLinkId}`, storage,
  };
  const uiPreferences = createUiPreferences(storage);
  const recovery = __NC_BUNDLED__ ? new RecoverySession({ access, storage,
    origin: window.location.origin, compatibleVersion: WEB_COMPAT_VERSION,
    adoptScope: scope => uiPreferences.setRecoveryScope(scope),
    identity: (signal) => runOperation(probe, { ...whoamiOperation(), signal }, undefined),
    version: (signal) => runOperation(probe, { ...serverVersionOperation(), signal }, unauthorized),
    logout: (signal) => runOperation(probe, { ...logoutOperation(), signal }, undefined),
    clear: () => clearSessionArtifacts(client, cursorStore, runtime), online: () => navigator.onLine, visible: () => !document.hidden,
  }, { input: takeScanInput(window, window.location.origin, Date.now()), pairing: createScanPairingPort(base) }) : undefined;
  const buildRouter = (shareScope: ShareScope | null = null) => createAppRouter({
    basepath: shareRoute?.basepath, shareScope, shareLinkId,
    prepareReport,
    transport,
    unauthorized,
    client,
    cards: { registry, host },
    recentFiles: createRecentFileHistory(storage),
    uiPreferences,
    onSignOut: () => {
      if (recovery) { void recovery.signOut(); return; }
      void runOperation(transport, logoutOperation(), unauthorized).finally(() => {
        clearSessionArtifacts(client, cursorStore, runtime);
        browser.reload();
      });
    },
  });
  const mounted = createRoot(root);
  mounted.render(<ProductionApp transport={transport} unauthorized={unauthorized} client={client}
    runtime={runtime} cursorStore={cursorStore} router={shareLinkId === undefined ? buildRouter() : buildRouter} recovery={recovery} shareLinkId={shareLinkId}
    renderLogin={(retry) => shareLinkId !== undefined
      ? <ShareUnlockGate initialToken={initialShareToken} shareLinkId={shareLinkId} transport={unlockTransport} retry={retry} />
      : __NC_BUNDLED__
      ? recovery?.scanOnly ? <BundledConnectionNotice kind="pairing"><p role="alert">{recovery.access.read().detail}</p></BundledConnectionNotice>
      : <BundledLoginPage message={recovery?.access.read().detail}
        onManualEntry={() => recovery!.cancelAuthentication()}
        verifyPairing={recovery?.blocked() ? () => { void recovery.verifyNewSession(); } : undefined}
        login={(username, password, signal) => loginForRecovery(probe, recovery!, username, password, signal)}
        reload={() => { /* verified session mounts directly */ }} />
      : <LoginPage login={(username, password, signal) => loginWithTransport(transport, username, password, signal)} reload={browser.reload} />}
    renderError={(retry) => __NC_BUNDLED__
      ? <BundledConnectionNotice kind="unreachable"><button type="button" onClick={retry}>重试连接</button></BundledConnectionNotice>
      : <main><p>Could not check your session.</p><button type="button" onClick={retry}>Try again</button></main>}
    renderEventBridge={(server) => recovery
      ? <RecoveryEventBridge key={`${recovery.scopeRevision}:${server.syncEventVersion}`} createEvents={createRecoveryEvents}
        recovery={recovery} client={client} version={server} />
      : <EventBridge client={client} stream={events!.stream}
        syncEventVersion={server.syncEventVersion} dbInstanceId={server.dbInstanceId} cursor={cursorStore} />}
  />);
}

/** Scope resolution lives inside SessionGate: no route loaders run on a locked link. */
function AuthenticatedRouter({ router, shareLinkId, unauthorized }: Readonly<{
  router: AnyRouter | ((scope: ShareScope) => AnyRouter); shareLinkId?: string; unauthorized: UnauthorizedChannel;
}>) {
  const [resolved, setResolved] = useState<AnyRouter | null>(null);
  const [error, setError] = useState(false);
  useEffect(() => {
    if (typeof router !== 'function' || shareLinkId === undefined) return;
    const controller = new AbortController();
    void runOperation(createFetchTransport(), { ...shareAccessOperation(shareLinkId), signal: controller.signal }, undefined).then(access => {
      if (controller.signal.aborted) return;
      if (access.status === 'unlocked') setResolved(router(access.scope));
      else unauthorized.notify();
    }).catch((error: unknown) => {
      if (controller.signal.aborted) return;
      if (error instanceof ApiError && 'status' in error.failure && (error.failure.status === 404 || error.failure.status === 401)) unauthorized.notify();
      else setError(true);
    });
    return () => controller.abort();
  }, [router, shareLinkId, unauthorized]);
  if (error) return <p role="alert">无法检查分享链接。</p>;
  const ready = typeof router === 'function' ? resolved : router;
  return ready === null ? null : <RouterProvider router={ready} />;
}
