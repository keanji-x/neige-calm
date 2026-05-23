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

import {
  QueryClient,
  useIsRestoring,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query';
import { PersistQueryClientProvider } from '@tanstack/react-query-persist-client';
import { useEffect, type ReactNode } from 'react';
import { useState } from '../shared/state';
import { EventBridge } from './eventBridge';
import { ThemeProvider } from './theme';
import { buildPersistOptions, IDB_DB_NAME, PERSIST_MAX_AGE_MS } from '../api/persistConfig';
import { Dialog } from '../ui/Dialog/Dialog';
import { CalmApiError } from '../api/calm';
import {
  WEB_COMPAT_VERSION,
  fetchServerVersion,
  isCompatible,
  type ServerVersionInfo,
} from '../api/version';

/**
 * Default retry policy for React Query — one retry on transient failures,
 * but never on 401. A 401 means the session cookie is gone (expired,
 * server restart, owner logout from a sibling tab); retrying just delays
 * the SessionProvider's `onUnauthorized` cleanup → LoginPage bounce that
 * `request()` in `api/calm.ts` already fired, and stacks up a doomed
 * second request in the meantime. See issue #189.
 *
 * Exported so per-query overrides can compose the same policy (e.g. when
 * a call site sets `staleTime: 0` but still wants the auth-aware retry).
 */
export function retryUnless401(failureCount: number, error: unknown): boolean {
  if (error instanceof CalmApiError && error.status === 401) return false;
  return failureCount < 1;
}

/**
 * `localStorage` key under which we stash the last observed
 * `dbInstanceId`. Compared on every mount against the server's current
 * value — see `ServerCompatGate` below.
 */
export const DB_INSTANCE_ID_STORAGE_KEY = 'calm:db_instance_id';

/**
 * WS sync cursor storage key. Mirrors the private constant in
 * `api/events.ts`; we duplicate it here (rather than import) so the
 * cache-bust path doesn't pull the WS module into the providers'
 * import graph. The value MUST stay in sync with `events.ts`.
 *
 * Exported so tests can assert against the bust path's canonical key
 * without re-hardcoding the string literal.
 */
export const WS_CURSOR_STORAGE_KEY = 'calm:sync:cursor';

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      gcTime: PERSIST_MAX_AGE_MS,
      retry: retryUnless401,
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
      <QueryRestoreGate>
        {/*
         * ThemeProvider lives INSIDE AppProviders (so it shares the same
         * mount lifetime as everything else here) but OUTSIDE the RouterProvider
         * that `children` ultimately resolves to. Plugin iframe cards rendered
         * inside routes call `useTheme()`; they need a single provider above
         * the entire route tree so a theme flip is observable across every
         * route at once. See issue #22.
         *
         * EventBridge is mounted INSIDE ServerCompatGate (not as a sibling
         * of `children`). The bridge opens the `/api/events` WebSocket on
         * first render and forwards every frame into the React Query cache;
         * doing that before the compat check has run would let an
         * incompatible frontend talk to a kernel it can't parse. ServerCompat
         * Gate hard-blocks rendering its children (including the bridge)
         * until `/api/version` confirms `minWebCompatVersion ≤ WEB_COMPAT_
         * VERSION`. See issue #198, concern 1.
         */}
        <ThemeProvider>
          <ServerCompatGate>{children}</ServerCompatGate>
        </ThemeProvider>
      </QueryRestoreGate>
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
 * On the same mount, also watch `dbInstanceId`: a UUID v4 the server
 * mints once per process boot (NOT persisted to the DB). If the value
 * we stashed in `localStorage` on a previous load differs from what the
 * server reports now, the underlying sqlite DB has been recreated under
 * us (operator ran `make dev RESET_DB=1`, a fresh-migrations branch
 * swap, etc.) and our persisted React Query cache + WS cursor are
 * holding row ids that no longer exist. Wipe both stores and hard-
 * reload — silently, because there's no client-owned state to lose
 * (everything lives server-side) and a confirmation modal would be
 * pure friction. First-time visitors (no previous id) just store the
 * value and continue.
 *
 * If the query itself fails (network blip, server down), we render the
 * children — better to let the existing per-query error banners surface
 * the connectivity problem than to block the entire app on a transient
 * /api/version failure. The DB-instance check is no-op'd in that case
 * too: missing data means we have nothing to compare against.
 */
export function ServerCompatGate({ children }: { children: ReactNode }) {
  const q = useQuery<ServerVersionInfo>({
    queryKey: ['server-version'],
    queryFn: fetchServerVersion,
    // The whole point of this query is the mount-time snapshot; stale
    // data after a fresh load is not useful, so we never serve a cached
    // value to a new mount.
    staleTime: 0,
    gcTime: 0,
    // `/api/version` is a public endpoint so it never actually returns
    // 401, but share the same policy as the QueryClient default so the
    // intent is unambiguous to future readers.
    retry: retryUnless401,
  });
  const qc = useQueryClient();
  // `busted` flips to true once we've kicked off the cache wipe + reload
  // path. The browser will navigate away momentarily; rendering `null`
  // in the interim prevents a flash of children with the cleared cache.
  const [busted, setBusted] = useState<boolean>(false);

  useEffect(() => {
    const id = q.data?.dbInstanceId;
    if (!id) return;
    const previous = safeLocalStorageGet(DB_INSTANCE_ID_STORAGE_KEY);
    if (previous && previous !== id) {
      // Server DB has been recreated since our last paint. Clear every
      // persisted client-side artifact that could now reference dead ids:
      //   * React Query in-memory cache (paint-from-snapshot lives here)
      //   * IndexedDB the persister writes to (`IDB_DB_NAME`)
      //   * WS event cursor in `localStorage[WS_CURSOR_STORAGE_KEY]`
      // Then write the new id and hard-reload — any in-flight queries
      // hold references to the cleared cache, so the cleanest path is
      // to start the page over from scratch against the new state.
      qc.clear();
      safeLocalStorageRemove(WS_CURSOR_STORAGE_KEY);
      safeDeleteIDB(IDB_DB_NAME);
      safeLocalStorageSet(DB_INSTANCE_ID_STORAGE_KEY, id);
      setBusted(true);
      window.location.reload();
      return;
    }
    if (!previous) {
      // First-time visitor (or `localStorage` was wiped). Just remember
      // the current id so the next boot has something to compare against.
      safeLocalStorageSet(DB_INSTANCE_ID_STORAGE_KEY, id);
    }
  }, [q.data?.dbInstanceId, qc]);

  if (q.data && !isCompatible(q.data)) {
    return <RefreshRequiredOverlay server={q.data} />;
  }
  if (busted) {
    // Reload is pending — render nothing rather than briefly flashing
    // the children with an empty cache.
    return null;
  }
  // EventBridge only mounts once compat is confirmed (q.data is set and
  // isCompatible). The `q.data` guard also gives us `syncEventVersion`,
  // which the bridge wires into the stream so future-eventVersion frames
  // are dropped without advancing the replay cursor. See issue #198.
  //
  // While `/api/version` is still in flight (cold-start), we render
  // children eagerly so the route tree paints from cached data — but
  // EventBridge stays unmounted, so no WS attempt happens until we know
  // the server is compatible. Once the query resolves on a compatible
  // server, the bridge mounts and opens the socket.
  //
  // Issue #198 followup (PR #215): children that call `sharedEventStream()`
  // during this in-flight window (e.g. `useConnectionState`, codex's hook
  // listener) USED TO trigger the singleton's auto-`start()`, opening a
  // socket before the bridge ran. The singleton is now inert until the
  // bridge calls `start()` explicitly, so the WS is genuinely not opened
  // before the compat verdict lands — the documented invariant above
  // holds verbatim, not merely in practice.
  return (
    <>
      {q.data ? <EventBridge syncEventVersion={q.data.syncEventVersion} /> : null}
      {children}
    </>
  );
}

// ---------------------------------------------------------------------------
// `localStorage` / `indexedDB` access helpers.
//
// All three storage APIs can throw or be undefined in degraded environments
// (private mode / disabled-storage browsers, SSR, tests that haven't
// installed a jsdom polyfill). The try/catch wrappers below are the
// difference between a graceful no-op and a runtime crash that takes the
// whole app down.
//
// In the degraded case, the cache-bust feature simply doesn't work — the
// user gets the same broken behavior they had pre-PR. That's a strictly
// better outcome than crashing the providers tree.
// ---------------------------------------------------------------------------

function safeLocalStorageGet(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function safeLocalStorageSet(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* private mode / quota — degrade silently */
  }
}

function safeLocalStorageRemove(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {
    /* private mode / quota — degrade silently */
  }
}

function safeDeleteIDB(name: string): void {
  try {
    indexedDB.deleteDatabase(name);
  } catch {
    /* IDB unavailable — degrade silently */
  }
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
