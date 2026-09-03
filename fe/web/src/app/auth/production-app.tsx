import { QueryClient } from '@tanstack/react-query';
import { RouterProvider, type AnyRouter } from '@tanstack/react-router';
import { StrictMode, type ReactNode } from 'react';
import { createRoot } from 'react-dom/client';
import { createUnauthorizedChannel, type UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { IDB_DB_NAME } from '../../../../core/keys/storage.ts';
import { LoginPage } from '../../features/auth/login-page/public.tsx';
import { loginWithTransport } from './login.ts';
import { clearSessionArtifacts, SessionGate } from './session-gate.tsx';
import { createBrowserEventComposition } from '../composition.ts';
import { EventBridge } from '../events/event-bridge.tsx';
import { AppProviders, type ProviderRuntime } from '../providers/public.tsx';
import { logoutOperation, runOperation, serverVersionOperation } from '../providers/queries.ts';
import { createFetchTransport } from '../providers/transport.ts';
import { createCardFilesPort } from '../providers/directory.ts';
import { createAppRouter } from '../router/public.tsx';
import { createCardHost, createCardRegistry } from '../../systems/cards/public.js';
import { bootCards } from '../cards.ts';

export function ProductionApp({ transport, unauthorized, client, runtime, cursorStore, router, renderEventBridge,
  renderLogin, renderError }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; client: QueryClient; runtime: ProviderRuntime;
  cursorStore: Parameters<typeof AppProviders>[0]['cursorStore']; router: AnyRouter;
  renderEventBridge?: Parameters<typeof AppProviders>[0]['renderEventBridge'];
  renderLogin: () => ReactNode; renderError: (retry: () => void) => ReactNode;
}>) {
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
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => queueMicrotask(task) }, { report: console.error });
  const transport = createFetchTransport();
  const client = new QueryClient();
  const events = createBrowserEventComposition({ storage: browser.storage, transport, unauthorizedChannel: unauthorized });
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
  const host = createCardHost(registry, { files: createCardFilesPort(transport, unauthorized) });
  const runtime: ProviderRuntime = {
    fetchVersion: () => runOperation(transport, serverVersionOperation(), unauthorized),
    reload: browser.reload, deleteDatabase: browser.deleteDatabase,
    idbDatabaseName: IDB_DB_NAME, storage: browser.storage,
  };
  const router = createAppRouter({ transport, unauthorized, client, cards: { registry, host }, onSignOut: () => {
    void runOperation(transport, logoutOperation(), unauthorized).finally(() => {
      clearSessionArtifacts(client, events.store, runtime);
      browser.reload();
    });
  } });
  createRoot(root).render(<ProductionApp transport={transport} unauthorized={unauthorized} client={client}
    runtime={runtime} cursorStore={events.store} router={router}
    renderLogin={() => <LoginPage login={(username, password) => loginWithTransport(transport, username, password)} reload={browser.reload} />}
    renderError={(retry) => <main><p>Could not check your session.</p><button type="button" onClick={retry}>Try again</button></main>}
    renderEventBridge={(server) => <EventBridge client={client} stream={events.stream}
      syncEventVersion={server.syncEventVersion} dbInstanceId={server.dbInstanceId} cursor={events.store} />}
  />);
}
