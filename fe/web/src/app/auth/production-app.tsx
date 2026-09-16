import { QueryClient } from '@tanstack/react-query';
import { RouterProvider, type AnyRouter } from '@tanstack/react-router';
import { StrictMode, type ReactNode } from 'react';
import { RecoveryGate } from './recovery-gate.tsx';
import { RecoverySession } from '../../systems/recovery/session.ts';
import { createRecoveryUnauthorizedChannel } from '../../systems/recovery/unauthorized.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { whoamiOperation } from '../../../../core/api/auth.ts';
import { createRoot } from 'react-dom/client';
import { createUnauthorizedChannel, type UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { IDB_DB_NAME } from '../../../../core/keys/storage.ts';
import { LoginPage } from '../../features/auth/login-page/public.tsx';
import { loginWithTransport } from './login.ts';
import { clearSessionArtifacts, SessionGate } from './session-gate.tsx';
import { createBrowserEventComposition } from '../composition.ts';
import { EventBridge } from '../events/event-bridge.tsx';
import { AppProviders, WEB_COMPAT_VERSION, type ProviderRuntime } from '../providers/public.tsx';
import { logoutOperation, runOperation, serverVersionOperation } from '../providers/queries.ts';
import { createFetchTransport } from '../providers/transport.ts';
import { createCardFilesPort } from '../providers/directory.ts';
import { createAppRouter } from '../router/public.tsx';
import { createUiPreferences } from '../providers/ui-preferences.tsx';
import { createRecentFileHistory } from '../providers/recent-files.ts';
import { createCardHost, createCardRegistry } from '../../systems/cards/public.js';
import { bootCards } from '../cards.ts';
import { BundledConnectionNotice, BundledLoginPage } from './bundled-connection.tsx';

export function ProductionApp({ transport, unauthorized, client, runtime, cursorStore, router, renderEventBridge,
  renderLogin, renderError, recovery }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; client: QueryClient; runtime: ProviderRuntime;
  cursorStore: Parameters<typeof AppProviders>[0]['cursorStore']; router: AnyRouter;
  renderEventBridge?: Parameters<typeof AppProviders>[0]['renderEventBridge'];
  recovery?: RecoverySession;
  renderLogin: () => ReactNode; renderError: (retry: () => void) => ReactNode;
}>) {
  if (__NC_BUNDLED__) return <StrictMode><RecoveryGate transport={transport} unauthorized={unauthorized} client={client}
    runtime={runtime} cursorStore={cursorStore} renderLogin={renderLogin} renderEventBridge={renderEventBridge} recovery={recovery}>
    <RouterProvider router={router} />
  </RecoveryGate></StrictMode>;
  return <StrictMode><SessionGate transport={transport} unauthorized={unauthorized} client={client}
    runtime={runtime} cursorStore={cursorStore}
    renderLogin={renderLogin} renderError={renderError}>
    <AppProviders client={client} runtime={runtime} cursorStore={cursorStore} renderEventBridge={renderEventBridge}>
      <RouterProvider router={router} />
    </AppProviders>
  </SessionGate></StrictMode>;
}

export function mountProductionApp(root: HTMLElement, browser: Readonly<{
  storage: Storage; reload: () => void; deleteDatabase: (name: string) => void;
}>): void {
  const access = new RecoveryAccess();
  const scheduler = { enqueue: (task: () => void) => queueMicrotask(task) };
  const unauthorized = __NC_BUNDLED__ ? createRecoveryUnauthorizedChannel(access, scheduler)
    : createUnauthorizedChannel(scheduler, { report: console.error });
  const base = createFetchTransport();
  const guarded = createRecoveryTransports(base, access);
  const transport = __NC_BUNDLED__ ? guarded.business : base;
  const probe = __NC_BUNDLED__ ? guarded.probe : base;
  const client = new QueryClient();
  const events = createBrowserEventComposition({ storage: browser.storage, transport: probe, unauthorizedChannel: unauthorized,
    ...(__NC_BUNDLED__ ? { probeUnauthorized: () => { recovery?.resume(); return Promise.resolve(); } } : {}),
  });
  // The one place the card runtime is assembled. `bootCards` is called exactly
  // once, on this instance — there is no module-level registry and no
  // module-level "already registered" guard (`INV-CARD-224` is retired); a
  // second boot would be a second registry, which is what the contract test
  // pins.
  const registry = createCardRegistry();
  bootCards(registry);
  // The card runtime's one I/O capability: the filesystem reads a card may
  // make, built from this app's transport and its 401 channel so a card's read
  // hits the same session handling as every other read (see `CardFilesPort`).
  const host = createCardHost(registry, { files: createCardFilesPort(transport, unauthorized), ...(__NC_BUNDLED__ ? { recovery: access } : {}) });
  const runtime: ProviderRuntime = {
    fetchVersion: () => runOperation(probe, serverVersionOperation(), unauthorized),
    reload: browser.reload, deleteDatabase: browser.deleteDatabase,
    idbDatabaseName: IDB_DB_NAME, storage: browser.storage,
  };
  const uiPreferences = createUiPreferences(browser.storage);
  const recovery = __NC_BUNDLED__ ? new RecoverySession({ access, storage: browser.storage,
    origin: window.location.origin, compatibleVersion: WEB_COMPAT_VERSION,
    adoptScope: scope => uiPreferences.setRecoveryScope(scope),
    identity: (signal) => runOperation(probe, { ...whoamiOperation(), signal }, undefined),
    version: (signal) => runOperation(probe, { ...serverVersionOperation(), signal }, unauthorized),
    logout: (signal) => runOperation(probe, { ...logoutOperation(), signal }, undefined),
    clear: () => clearSessionArtifacts(client, events.store, runtime), online: () => navigator.onLine, visible: () => !document.hidden,
  }) : undefined;
  if (recovery) events.stream.onConnectionState((state) => recovery?.events(state));
  const router = createAppRouter({
    transport,
    unauthorized,
    client,
    cards: { registry, host },
    recentFiles: createRecentFileHistory(browser.storage),
    uiPreferences,
    onSignOut: () => {
      if (recovery) { void recovery.signOut(); return; }
      void runOperation(transport, logoutOperation(), unauthorized).finally(() => {
        clearSessionArtifacts(client, events.store, runtime);
        browser.reload();
      });
    },
  });
  createRoot(root).render(<ProductionApp transport={transport} unauthorized={unauthorized} client={client}
    runtime={runtime} cursorStore={events.store} router={router} recovery={recovery}
    renderLogin={() => __NC_BUNDLED__
      ? <BundledLoginPage message={recovery?.access.read().detail}
        verifyPairing={recovery?.blocked() ? () => { void recovery.verifyNewSession(); } : undefined}
        login={async (username, password) => {
        const result = await loginWithTransport(probe, username, password);
        return result === null ? null : await recovery!.verifyNewSession(result.sessionId);
      }} reload={() => { /* verified session mounts directly */ }} />
      : <LoginPage login={(username, password) => loginWithTransport(transport, username, password)} reload={browser.reload} />}
    renderError={(retry) => __NC_BUNDLED__
      ? <BundledConnectionNotice kind="unreachable"><button type="button" onClick={retry}>重试连接</button></BundledConnectionNotice>
      : <main><p>Could not check your session.</p><button type="button" onClick={retry}>Try again</button></main>}
    renderEventBridge={(server) => <EventBridge client={client} stream={events.stream}
      syncEventVersion={server.syncEventVersion} dbInstanceId={server.dbInstanceId} cursor={events.store} />}
  />);
}
