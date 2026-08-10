// Code-based TanStack Router setup.
//
// The whole tree is built inside a factory: `createRoute`/`createRouter` at
// module scope would be module runtime state, and injecting the transport and
// the QueryClient is what lets a test drive a real router without touching a
// module singleton.

import {
  createRootRoute, createRoute, createRouter, type AnyRoute,
} from '@tanstack/react-router';
import type { QueryClient } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { TodayPage } from '../../features/today/public.tsx';
import { prefetchCoveList, useWorkspace } from '../providers/queries.ts';
import { AppShell } from '../shell/public.tsx';
import { useGo } from './navigation.ts';
import { PendingRoute } from './pending-route.tsx';

export type AppRouterDeps = Readonly<{ transport: ApiTransportPort; client: QueryClient }>;

export function createRouteTree({ transport, client }: AppRouterDeps): AnyRoute {
  const rootRoute = createRootRoute({ component: () => <AppShell transport={transport} /> });

  const indexRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/',
    /**
     * INV-APP-084 — the index loader primes **only** the coves list. The
     * cove → waves fan-out stays lazy inside the page (`useQueries` in
     * `useWorkspace`); awaiting it here would let one slow cove block the
     * whole calendar behind the route commit.
     */
    loader: () => prefetchCoveList(client, transport),
    component: () => <TodayRoute transport={transport} />,
  });

  const coveRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/cove/$coveId',
    component: () => <PendingRoute label="Cove" owner="features/cove" />,
  });

  const waveRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/wave/$waveId',
    component: () => <PendingRoute label="Wave" owner="features/wave" />,
  });

  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/settings',
    component: () => <PendingRoute label="Settings" owner="features/settings" />,
  });

  return rootRoute.addChildren([indexRoute, coveRoute, waveRoute, settingsRoute]);
}

export function createAppRouter(deps: AppRouterDeps) {
  return createRouter({ routeTree: createRouteTree(deps), defaultPreload: false });
}

function TodayRoute({ transport }: { transport: ApiTransportPort }) {
  const workspace = useWorkspace(transport);
  const go = useGo();
  return (
    <TodayPage
      waves={workspace.waves}
      coves={workspace.coves}
      onOpenWave={(waveId) => go({ name: 'wave', waveId })}
    />
  );
}
