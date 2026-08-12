// Route targets and the single navigation exit.

import { useNavigate, useRouterState } from '@tanstack/react-router';
import { useCallback } from 'react';

export type NavTarget =
  | Readonly<{ name: 'today' }>
  | Readonly<{ name: 'cove'; coveId: string }>
  | Readonly<{ name: 'wave'; waveId: string }>
  | Readonly<{ name: 'settings' }>;

export function pathFor(target: NavTarget): string {
  switch (target.name) {
    case 'today': return '/';
    case 'cove': return `/cove/${target.coveId}`;
    case 'wave': return `/wave/${target.waveId}`;
    case 'settings': return '/settings';
  }
}

/**
 * INV-A11Y-061 — navigation is **intentionally** a `<button>` plus this
 * callback; the product does not spread native `<a href>` links around
 * (a11y contract §3.2, "We don't have many links today"). Mixing the two forks
 * Tab and activation semantics — Enter vs Space behave differently on links
 * and buttons — so a row that navigates must be a button. Anything that wants
 * a real URL affordance has to change this decision deliberately, not by
 * dropping one `<a>` into one row.
 */
export function useGo(): (target: NavTarget) => void {
  const navigate = useNavigate();
  return useCallback((target: NavTarget) => {
    void navigate({ to: pathFor(target) });
  }, [navigate]);
}

export function useCurrentPath(): string {
  return useRouterState({ select: (state) => state.location.pathname });
}

/**
 * Reads a single path segment straight off the URL.
 *
 * TanStack's `useParams({ strict: false })` widens to `any` outside a typed
 * route context, which the `no-unsafe-*` rules reject — and silencing them
 * would hand every route component an unchecked id. Parsing here keeps the id
 * a `string` and keeps the prefix table next to `pathFor`, so the two cannot
 * drift apart.
 */
export function useRouteParam(prefix: '/cove/' | '/wave/'): string | undefined {
  const path = useCurrentPath();
  if (!path.startsWith(prefix)) return undefined;
  const segment = path.slice(prefix.length).split('/', 1)[0];
  return segment === '' ? undefined : decodeURIComponent(segment);
}
