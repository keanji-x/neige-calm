// Route targets and the single navigation exit.

import { useNavigate, useRouterState } from '@tanstack/react-router';
import { useCallback } from 'react';

export type NavTarget =
  | Readonly<{ name: 'today' }>
  | Readonly<{ name: 'cove'; coveId: string }>
  /**
   * `blockId` lands the reader on one block of the wave's report (§8.3).
   * `cardId` opens the card-grid overlay on that card (`?card=`).
   */
  | Readonly<{ name: 'wave'; waveId: string; blockId?: string; cardId?: string }>
  | Readonly<{ name: 'settings' }>;

export type GoOptions = Readonly<{ replace?: boolean }>;

export function pathFor(target: NavTarget): string {
  switch (target.name) {
    case 'today': return '/';
    case 'cove': return `/cove/${encodeURIComponent(target.coveId)}`;
    case 'wave': return `/wave/${encodeURIComponent(target.waveId)}`;
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
export function useGo(): (target: NavTarget, options?: GoOptions) => void {
  const navigate = useNavigate();
  return useCallback((target: NavTarget, options?: GoOptions) => {
    // The block anchor rides in the hash rather than in component state,
    // because it has to survive the navigation that carries it: the wave route
    // remounts per wave, so state set before `go` would be discarded by the
    // very move it was describing. A hash also makes the deep link real —
    // pasting one lands on the same paragraph.
    const hash = target.name === 'wave' ? target.blockId : undefined;
    // Wave search is always explicit so a card query cannot leak onto today
    // or a different wave, and so clearing `cardId` actually drops `?card=`.
    const search = target.name === 'wave' && target.cardId !== undefined
      ? { card: target.cardId }
      : {};
    void navigate({ to: pathFor(target), hash, search, replace: options?.replace });
  }, [navigate]);
}

/**
 * The card the current URL points at, or `null`.
 *
 * Duplicates, empty values, and non-strings are rejected by reading the raw
 * query string (`getAll('card')`), not a parser that may have already folded
 * repeated keys.
 */
export function useRouteCardId(): string | null {
  return useRouterState({ select: (state) => cardIdFromLocation(state.location) });
}

export function cardIdFromLocation(location: Readonly<{
  searchStr?: string;
  search?: unknown;
  href?: string;
}>): string | null {
  if (typeof location.searchStr === 'string' && location.searchStr !== '') {
    return cardIdFromSearchString(location.searchStr);
  }
  if (typeof location.search === 'string') return cardIdFromSearchString(location.search);
  if (typeof location.href === 'string' && location.href.includes('?')) {
    const query = location.href.split('?')[1]?.split('#')[0] ?? '';
    const fromHref = cardIdFromSearchString(query);
    if (fromHref !== null) return fromHref;
    if (new URLSearchParams(query).getAll('card').length !== 1) return null;
  }
  if (typeof location.search === 'object' && location.search !== null) {
    const card = (location.search as { card?: unknown }).card;
    return typeof card === 'string' && card !== '' ? card : null;
  }
  return null;
}

export function cardIdFromSearchString(searchStr: string): string | null {
  const raw = searchStr.startsWith('?') ? searchStr.slice(1) : searchStr;
  if (raw === '') return null;
  const values = new URLSearchParams(raw).getAll('card');
  if (values.length !== 1) return null;
  const value = values[0];
  return value === undefined || value === '' ? null : value;
}

/** The block anchor the current URL points at, or `null`. */
export function useRouteHash(): string | null {
  const hash = useRouterState({ select: (state) => state.location.hash });
  return hash === '' ? null : hash;
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
  return routeParamFromPath(path, prefix);
}

export function routeParamFromPath(path: string, prefix: '/cove/' | '/wave/'): string | undefined {
  if (!path.startsWith(prefix)) return undefined;
  const segment = path.slice(prefix.length).split('/', 1)[0];
  if (segment === '') return undefined;
  try {
    return decodeURIComponent(segment);
  } catch {
    return undefined;
  }
}
