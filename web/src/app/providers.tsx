// AppProviders — single QueryClientProvider wrapping the app.
//
// One shared QueryClient lives at module scope so it's stable across
// re-renders and importable by router loaders (M3 will start moving
// data fetching here). `EventBridge` lives inside the provider so it
// can grab the queryClient via context and translate WS events into
// cache invalidations — the kernel's WS bus drives the UI's freshness
// without anyone touching component state directly.
//
// Devtools only mount in dev (Vite's `import.meta.env.DEV`).

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ReactQueryDevtools } from '@tanstack/react-query-devtools';
import type { ReactNode } from 'react';
import { EventBridge } from './eventBridge';

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      retry: 1,
      refetchOnWindowFocus: false,
    },
  },
});

export function AppProviders({ children }: { children: ReactNode }) {
  return (
    <QueryClientProvider client={queryClient}>
      <EventBridge />
      {children}
      {import.meta.env.DEV && (
        <ReactQueryDevtools initialIsOpen={false} buttonPosition="bottom-left" />
      )}
    </QueryClientProvider>
  );
}
