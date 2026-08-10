import { QueryClient } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { AppProviders, type ProviderRuntime } from './app/providers/public.tsx';
import { runOperation, serverVersionOperation } from './app/providers/queries.ts';
import { createFetchTransport } from './app/providers/transport.ts';
import { createAppRouter } from './app/router/public.tsx';

const root = document.getElementById('root');

if (!root) throw new Error('Missing #root mount point');

const transport = createFetchTransport();
const client = new QueryClient();
const router = createAppRouter({ transport, client });

const runtime: ProviderRuntime = {
  fetchVersion: () => runOperation(transport, serverVersionOperation()),
  reload: () => { window.location.reload(); },
  deleteDatabase: (name) => { indexedDB.deleteDatabase(name); },
  idbDatabaseName: 'calm',
  storage: window.localStorage,
};

createRoot(root).render(
  <StrictMode>
    <AppProviders client={client} runtime={runtime}>
      <RouterProvider router={router} />
    </AppProviders>
  </StrictMode>,
);
