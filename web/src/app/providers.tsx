// AppProviders — single QueryClientProvider wrapping the app.
//
// One shared QueryClient lives at module scope so it's stable across
// re-renders and importable by router loaders (M3 will start moving
// data fetching here). Defaults are conservative — `useKernel` still
// owns the live WS data flow, so we don't want react-query to be
// aggressive about background refetches yet.
//
// Devtools only mount in dev (Vite's `import.meta.env.DEV`).

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ReactQueryDevtools } from '@tanstack/react-query-devtools';
import type { ReactNode } from 'react';

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
      {children}
      {import.meta.env.DEV && (
        <ReactQueryDevtools initialIsOpen={false} buttonPosition="bottom-left" />
      )}
    </QueryClientProvider>
  );
}
