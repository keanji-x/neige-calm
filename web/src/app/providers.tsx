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
// Issue #45: frontend ↔ backend skew check.
// We mount one `useQuery(['server-version'])` here — the smallest scope
// that wraps every route, every modal, every WS-driven UI write — and
// compare its `minWebCompatVersion` against the frontend's
// `WEB_COMPAT_VERSION`. On mismatch we paint a hard-block overlay over
// the whole tree directing the user to refresh. Single check on mount;
// no polling. See `docs/upgrade-stability.md` (Tier B).
//
// Devtools only mount in dev (Vite's `import.meta.env.DEV`).

import { QueryClient, useIsRestoring, useQuery } from '@tanstack/react-query';
import { ReactQueryDevtools } from '@tanstack/react-query-devtools';
import { PersistQueryClientProvider } from '@tanstack/react-query-persist-client';
import type { ReactNode } from 'react';
import { EventBridge } from './eventBridge';
import { buildPersistOptions, PERSIST_MAX_AGE_MS } from '../api/persistConfig';
import { Dialog } from '../ui/Dialog/Dialog';
import {
  WEB_COMPAT_VERSION,
  fetchServerVersion,
  isCompatible,
  type ServerVersionInfo,
} from '../api/version';

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
      <QueryRestoreGate>
        <ServerCompatGate>{children}</ServerCompatGate>
      </QueryRestoreGate>
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

/**
 * Hard-block the app if the running backend's `minWebCompatVersion` is
 * higher than this bundle's `WEB_COMPAT_VERSION`. The check runs once on
 * mount (no `refetchInterval`); on mismatch we render an overlay over
 * the whole tree directing the user to refresh.
 *
 * If the query itself fails (network blip, server down), we render the
 * children — better to let the existing per-query error banners surface
 * the connectivity problem than to block the entire app on a transient
 * /api/version failure.
 */
function ServerCompatGate({ children }: { children: ReactNode }) {
  const q = useQuery<ServerVersionInfo>({
    queryKey: ['server-version'],
    queryFn: fetchServerVersion,
    // The whole point of this query is the mount-time snapshot; stale
    // data after a fresh load is not useful, so we never serve a cached
    // value to a new mount.
    staleTime: 0,
    gcTime: 0,
    retry: 1,
  });

  if (q.data && !isCompatible(q.data)) {
    return <RefreshRequiredOverlay server={q.data} />;
  }
  return <>{children}</>;
}

/** Hard-block modal directing the user to refresh after a backend skew is
 *  detected. Built on the shared `<Dialog>` primitive so it inherits the
 *  focus trap, Esc handling, background inert, and focus restore contracts
 *  the rest of the app relies on. There is only one resolution path —
 *  reload the page — so Esc and overlay-click route through the same
 *  `Refresh now` handler. */
export function RefreshRequiredOverlay({ server }: { server: ServerVersionInfo }) {
  const refresh = () => window.location.reload();
  return (
    <Dialog open onClose={refresh} title="Please refresh">
      <p>
        A new version of neige-calm is running on the server (compat v
        {server.minWebCompatVersion}). Your browser tab is loaded with an
        older build (compat v{WEB_COMPAT_VERSION}). Refresh this page to
        continue.
      </p>
      <button type="button" className="go" onClick={refresh}>
        Refresh now
      </button>
    </Dialog>
  );
}
