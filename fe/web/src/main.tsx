import { QueryClient } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { createUnauthorizedChannel } from '../../core/api/unauthorized.ts';
import { AppProviders, type ProviderRuntime } from './app/providers/public.tsx';
import { createEventComposition } from './app/composition.ts';
import { EventBridge } from './app/events/event-bridge.tsx';
import { logoutOperation, runOperation, serverVersionOperation } from './app/providers/queries.ts';
import { createFetchTransport } from './app/providers/transport.ts';
import { createAppRouter } from './app/router/public.tsx';

const root = document.getElementById('root');

if (!root) throw new Error('Missing #root mount point');

const transport = createFetchTransport();
const client = new QueryClient();
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => queueMicrotask(task) }, { report: console.error });
const events = createEventComposition({
  storage: window.localStorage,
  transport,
  onUnauthorized: () => unauthorized.notify(),
});
const router = createAppRouter({
  transport,
  client,
  onSignOut: () => {
    // Reload rather than clearing caches by hand: a fresh document re-probes
    // the session and restarts every persisted store from a known state.
    void runOperation(transport, logoutOperation()).finally(() => { window.location.reload(); });
  },
});

const runtime: ProviderRuntime = {
  fetchVersion: () => runOperation(transport, serverVersionOperation()),
  reload: () => { window.location.reload(); },
  deleteDatabase: (name) => { indexedDB.deleteDatabase(name); },
  idbDatabaseName: 'calm',
  storage: window.localStorage,
};

createRoot(root).render(
  <StrictMode>
    <AppProviders
      client={client}
      runtime={runtime}
      cursorStore={events.store}
      renderEventBridge={(server) => (
        <EventBridge
          client={client}
          stream={events.stream}
          syncEventVersion={server.syncEventVersion}
          dbInstanceId={server.dbInstanceId}
          cursor={events.store}
        />
      )}
    >
      <RouterProvider router={router} />
    </AppProviders>
  </StrictMode>,
);
