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

/** Full-viewport modal that blocks interaction underneath and directs the
 *  user to refresh. Pure JSX — no modal library, no shadow DOM. */
export function RefreshRequiredOverlay({ server }: { server: ServerVersionInfo }) {
  return (
    // TODO(#60): migrate this overlay onto the `<Dialog>` primitive
    // (`web/src/ui/Dialog`). The overlay deliberately runs *outside* the
    // app's normal QueryClient / portal infrastructure (it's the
    // refresh-required hard block) so the migration needs a story for
    // standalone, infra-free Dialog mounting before it's safe. Tracked as
    // unfinished slice-1 cleanup in issue #60 (no-raw-primitive-role survey).
    <div
      // eslint-disable-next-line neige-calm/no-raw-primitive-role
      role="dialog"
      aria-modal="true"
      aria-labelledby="refresh-required-title"
      style={{
        position: 'fixed',
        inset: 0,
        zIndex: 9999,
        background: 'rgba(0, 0, 0, 0.5)',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        padding: 16,
      }}
    >
      <div
        style={{
          background: 'var(--bg, #fff)',
          color: 'var(--fg, #111)',
          padding: '24px 28px',
          borderRadius: 8,
          maxWidth: 420,
          boxShadow: '0 8px 32px rgba(0, 0, 0, 0.25)',
        }}
      >
        <h2 id="refresh-required-title" style={{ marginTop: 0 }}>
          Please refresh
        </h2>
        <p>
          A new version of neige-calm is running on the server (compat v
          {server.minWebCompatVersion}). Your browser tab is loaded with an
          older build (compat v{WEB_COMPAT_VERSION}). Refresh this page to
          continue.
        </p>
        <button
          type="button"
          className="go"
          onClick={() => window.location.reload()}
          style={{ marginTop: 8 }}
        >
          Refresh now
        </button>
      </div>
    </div>
  );
}
