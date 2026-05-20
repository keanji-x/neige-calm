// AppProviders — single QueryClientProvider wrapping the app.
//
// One shared QueryClient lives at module scope so it's stable across
// re-renders and importable by router loaders (M3 will start moving
// data fetching here). `EventBridge` lives inside the provider so it
// can grab the queryClient via context and translate WS events into
// cache invalidations — the kernel's WS bus drives the UI's freshness
// without anyone touching component state directly.
//
// Scope F: cache persistence to IndexedDB.
// We swap the bare `QueryClientProvider` for `PersistQueryClientProvider`
// from `@tanstack/react-query-persist-client`. On boot it hydrates the
// in-memory cache from the persisted blob in IndexedDB; on cache writes
// it serializes back. Allowlist + buster + maxAge live in
// `api/persistConfig.ts` — see that file for the policy. The first paint
// after a fresh reload now shows cached coves/waves/overlays instantly
// while React Query refetches in the background per the normal
// staleTime / refetchOnMount rules; nothing about the online behavior
// changes, the persister just front-loads the data.
//
// Devtools only mount in dev (Vite's `import.meta.env.DEV`).

import { QueryClient, useIsRestoring } from '@tanstack/react-query';
import { ReactQueryDevtools } from '@tanstack/react-query-devtools';
import { PersistQueryClientProvider } from '@tanstack/react-query-persist-client';
import type { ReactNode } from 'react';
import { EventBridge } from './eventBridge';
import { buildPersistOptions, PERSIST_MAX_AGE_MS } from '../api/persistConfig';

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      gcTime: PERSIST_MAX_AGE_MS,
      retry: 1,
      refetchOnWindowFocus: false,
    },
  },
});

// Build persistOptions once at module scope so the IndexedDB connection
// isn't reopened on every AppProviders re-render.
const persistOptions = buildPersistOptions();

export function AppProviders({ children }: { children: ReactNode }) {
  return (
    <PersistQueryClientProvider client={queryClient} persistOptions={persistOptions}>
      <EventBridge />
      <QueryRestoreGate>{children}</QueryRestoreGate>
      {import.meta.env.DEV && (
        <ReactQueryDevtools initialIsOpen={false} buttonPosition="bottom-left" />
      )}
    </PersistQueryClientProvider>
  );
}

function QueryRestoreGate({ children }: { children: ReactNode }) {
  const isRestoring = useIsRestoring();
  return isRestoring ? null : children;
}
