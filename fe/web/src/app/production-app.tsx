import { QueryClient } from '@tanstack/react-query';
import { RouterProvider, type AnyRouter } from '@tanstack/react-router';
import { StrictMode, type ReactNode } from 'react';
import { createRoot } from 'react-dom/client';
import { createUnauthorizedChannel, type UnauthorizedChannel } from '../../../core/api/unauthorized.ts';
import type { ApiTransportPort } from '../../../core/api/types.ts';
import { LoginPage } from '../features/auth/login-page/public.tsx';
import { loginWithTransport } from './auth/login.ts';
import { SessionGate } from './auth/session-gate.tsx';
import { createBrowserEventComposition } from './composition.ts';
import { EventBridge } from './events/event-bridge.tsx';
import { AppProviders, type ProviderRuntime } from './providers/public.tsx';
import { logoutOperation, runOperation, serverVersionOperation } from './providers/queries.ts';
import { createFetchTransport } from './providers/transport.ts';
import { createAppRouter } from './router/public.tsx';

export function ProductionApp({ transport, unauthorized, client, runtime, cursorStore, router, renderEventBridge,
  renderLogin, renderError }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel; client: QueryClient; runtime: ProviderRuntime;
  cursorStore: Parameters<typeof AppProviders>[0]['cursorStore']; router: AnyRouter;
  renderEventBridge?: Parameters<typeof AppProviders>[0]['renderEventBridge'];
  renderLogin: () => ReactNode; renderError: (retry: () => void) => ReactNode;
}>) {
  return <StrictMode><SessionGate transport={transport} unauthorized={unauthorized} client={client}
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
  const router = createAppRouter({ transport, unauthorized, client, onSignOut: () => {
    void runOperation(transport, logoutOperation(), unauthorized).finally(browser.reload);
  } });
  const runtime: ProviderRuntime = {
    fetchVersion: () => runOperation(transport, serverVersionOperation(), unauthorized),
    reload: browser.reload, deleteDatabase: browser.deleteDatabase,
    idbDatabaseName: 'calm', storage: browser.storage,
  };
  createRoot(root).render(<ProductionApp transport={transport} unauthorized={unauthorized} client={client}
    runtime={runtime} cursorStore={events.store} router={router}
    renderLogin={() => <LoginPage login={(username, password) => loginWithTransport(transport, username, password)} reload={browser.reload} />}
    renderError={(retry) => <main><p>Could not check your session.</p><button type="button" onClick={retry}>Try again</button></main>}
    renderEventBridge={(server) => <EventBridge client={client} stream={events.stream}
      syncEventVersion={server.syncEventVersion} dbInstanceId={server.dbInstanceId} cursor={events.store} />}
  />);
}
