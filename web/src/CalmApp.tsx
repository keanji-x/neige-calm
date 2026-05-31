// CalmApp — the layout shell rendered by the router's root route.
//
// What's here: TitleBar, Sidebar, and the <Outlet /> where the matched
// route renders its page. URL drives selection (see `app/router.tsx`);
// this component holds no kernel data of its own.
//
// Kernel data flows through TanStack Query hooks (see `api/queries.ts`):
// every page fetches what it needs and the shared QueryClient
// deduplicates. WS-driven freshness is handled by `app/eventBridge.tsx`,
// mounted inside `AppProviders` so it sees the same QueryClient.
//
// What this component still owns:
//   * the Sidebar's data shape: it wants `Cove[]` and `Wave[]` (across
//     all coves) for the "running / waiting" badges. We fetch coves
//     once and fan out wave queries with `useQueries`, then adapt to
//     UI shapes inline. The result is shallow-stable enough for the
//     Sidebar; per-cove invalidations naturally roll up.
//
// Theme is no longer local to CalmApp — it lives in `app/theme.tsx`
// (`ThemeProvider` mounted by `AppProviders`) and is read via the
// `useTheme()` hook. The TitleBar no longer hosts a theme toggle; the
// only place to change theme is the Settings page's Appearance section
// (Light/Dark/System radio), reachable via the Sidebar avatar menu.
// See issue #22.

import { Suspense, useMemo } from 'react';
import { Outlet, useRouterState } from '@tanstack/react-router';
import { useQueries } from '@tanstack/react-query';
import { Sidebar } from './shared/components/Sidebar';
import { TitleBar } from './shared/components/TitleBar';
import { adaptCove, adaptWave } from './api/adapt';
import * as api from './api/calm';
import {
  queryKeys,
  useCovesQuery,
  useCreateCoveMutation,
  useDeleteCoveMutation,
  useDeleteWaveMutation,
  useOverlaysByKindQuery,
  useUpdateWaveMutation,
} from './api/queries';
import { useGo } from './app/navigation';
import { logout } from './api/auth';
import type { KernelOverlay } from './api/wire';
import type { Cove, Route as AppRoute, Wave } from './types';

export function CalmApp() {
  const go = useGo();

  // Derive the current AppRoute shape from the router's location so the
  // Sidebar's "highlight active" logic keeps working without props on
  // every route component. Subscribing via useRouterState ensures we
  // re-render on history changes (back / forward / programmatic nav).
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const route: AppRoute = useMemo(() => parseAppRoute(pathname), [pathname]);

  // ----- Sidebar data -----------------------------------------------------
  //
  // Sidebar wants a flat list of all waves so it can render per-cove
  // counts and the "Waiting on you" bucket. We fan out one query per
  // cove and adapt the results. Each query has its own cache entry, so
  // a single-cove invalidation only refetches that cove's wave list.

  const covesQ = useCovesQuery();
  // Memoise the fallback to a stable empty array — without this, the
  // `?? []` allocates a fresh `[]` on every render, which would make
  // `kernelCoves` (and any downstream memo keyed on it) change identity
  // every render. The eslint-plugin-react-hooks `exhaustive-deps` check
  // explicitly flags this pattern.
  //
  // Belt-and-suspenders for issue #175: the server already filters
  // `kind='system'` out of `GET /api/coves` by default, but we re-filter
  // here so a future regression on the server side (or a debug build
  // that opts into `?include_system=true`) never accidentally surfaces
  // the system cove in the sidebar.
  const kernelCoves = useMemo(
    () => (covesQ.data ?? []).filter((c) => c.kind === 'user'),
    [covesQ.data],
  );

  const waveQueries = useQueries({
    queries: kernelCoves.map((c) => ({
      queryKey: queryKeys.wavesInCove(c.id),
      queryFn: () => api.wavesInCove(c.id),
    })),
  });

  const coves: Cove[] = useMemo(() => kernelCoves.map(adaptCove), [kernelCoves]);

  // Workspace-wide wave overlays — one cheap query that the Sidebar
  // reads to render accurate per-wave status indicators ("Waiting on
  // you", "X running") for every cove, not just whichever wave the
  // user has currently opened. eventBridge invalidates this snapshot
  // on overlay.set/.deleted (and on wave/cove deletes where the kernel
  // may not cascade individual events).
  const waveOverlaysQ = useOverlaysByKindQuery('wave');

  const overlaysByWaveId = useMemo(() => {
    const m = new Map<string, KernelOverlay[]>();
    for (const o of waveOverlaysQ.data ?? []) {
      if (o.entity_kind !== 'wave') continue;
      const cur = m.get(o.entity_id);
      if (cur) cur.push(o);
      else m.set(o.entity_id, [o]);
    }
    return m;
  }, [waveOverlaysQ.data]);

  const waves: Wave[] = useMemo(() => {
    const out: Wave[] = [];
    for (const q of waveQueries) {
      if (!q.data) continue;
      for (const w of q.data) {
        out.push(adaptWave(w, overlaysByWaveId.get(w.id) ?? []));
      }
    }
    return out;
    // Stable-ish: depends on each query's data identity. React-Query
    // keeps data references stable across refetches when the payload
    // is structurally equal, so this re-derives only on real changes.
  }, [waveQueries, overlaysByWaveId]);

  const loading = covesQ.isLoading;
  const error = covesQ.error;

  const createCove = useCreateCoveMutation();
  const deleteCove = useDeleteCoveMutation();
  const deleteWave = useDeleteWaveMutation();
  const updateWave = useUpdateWaveMutation();

  // Sign-out (issue #189). POSTs `/api/auth/logout` which drops the
  // server-side session + clears the `calm-session` cookie. We then
  // reload so SessionProvider's whoami probe re-runs against the now-
  // anonymous cookie state and lands the user on `<LoginPage />`. The
  // reload is preferred over a pure in-memory state flip so every
  // persisted cache (React Query IDB, WS cursor, etc.) starts clean
  // under the next sign-in — matches the cache-bust path the
  // `fireUnauthorized` listener takes for 401s. (Logout itself doesn't
  // 401, so we have to fire the cleanup explicitly via the reload.)
  const handleSignOut = async () => {
    await logout();
    window.location.reload();
  };

  return (
    <div className="win">
      <TitleBar />
      <div className="stage">
        <Sidebar
          coves={coves}
          waves={waves}
          route={route}
          onGo={go}
          onCreateCove={async (name, color) => {
            await createCove.mutateAsync({ name, color });
          }}
          onDeleteCove={async (cId) => {
            try {
              await deleteCove.mutateAsync(cId);
              // Active-cove deletion: bounce to Today so we don't get
              // stranded on a now-missing /cove/:id route.
              if (route.name === 'cove' && route.coveId === cId) {
                go({ name: 'today' });
              }
            } catch (err) {
              console.warn('[Calm] cove delete failed:', err);
            }
          }}
          onDeleteWave={async (waveId) => {
            const wave = waves.find((w) => w.id === waveId);
            if (!wave) return;
            try {
              await deleteWave.mutateAsync({ id: waveId, coveId: wave.coveId });
              if (route.name === 'wave' && route.id === waveId) {
                go({ name: 'cove', coveId: wave.coveId });
              }
            } catch (err) {
              console.warn('[Calm] wave delete failed:', err);
            }
          }}
          onPinWave={async (waveId, pin) => {
            await updateWave.mutateAsync({
              id: waveId,
              body: { pinned_at: pin ? Date.now() : null },
            });
          }}
          onOpenSettings={() => go({ name: 'settings' })}
          onSignOut={handleSignOut}
        />
        <main className="page">
          <div className="scroll">
            {error && <ErrorBanner err={error} />}
            {loading ? (
              <LoadingShell />
            ) : (
              // Route page components are lazily imported in `app/router.tsx`,
              // so the first navigation to each route suspends while its
              // chunk downloads. One Suspense at the Outlet covers all of
              // them with a consistent fallback.
              <Suspense fallback={<RouteLoading />}>
                <Outlet />
              </Suspense>
            )}
          </div>
        </main>
      </div>
    </div>
  );
}

function parseAppRoute(pathname: string): AppRoute {
  if (pathname.startsWith('/cove/')) {
    const id = decodeURIComponent(pathname.slice('/cove/'.length).replace(/\/$/, ''));
    if (id) return { name: 'cove', coveId: id };
  }
  if (pathname.startsWith('/wave/')) {
    const id = decodeURIComponent(pathname.slice('/wave/'.length).replace(/\/$/, ''));
    if (id) return { name: 'wave', id };
  }
  if (pathname === '/settings' || pathname.startsWith('/settings/')) {
    return { name: 'settings' };
  }
  return { name: 'today' };
}

function LoadingShell() {
  return (
    <div className="col">
      <p className="synth">Connecting to calm-server…</p>
    </div>
  );
}

function RouteLoading() {
  // Briefly visible only on the very first navigation to a route whose
  // chunk hasn't been fetched yet. We deliberately match LoadingShell's
  // muted styling so the transition reads as "calm" rather than "spinner".
  return (
    <div className="col">
      <p className="synth">Loading…</p>
    </div>
  );
}

function ErrorBanner({ err }: { err: Error }) {
  return (
    <div className="col" style={{ color: 'var(--warn, #c00)', marginBottom: 12 }}>
      <p className="synth">
        Kernel error: {err.message}. The page reflects the last successful read.
      </p>
    </div>
  );
}

export default CalmApp;
