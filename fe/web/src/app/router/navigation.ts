// Route targets and the single navigation exit.

import { useNavigate, useRouter, useRouterState, type RouterHistory } from '@tanstack/react-router';
import { useCallback, useMemo } from 'react';

import { useTrackViews } from './track-view-state.tsx';
import type { MobilePanel, TrackSource, TrackSearch } from './track-search.ts';

import { parseWorkspaceRelativeFilePath } from '../../../../core/domain/report-file.ts';

export type { MobilePanel, TrackSource, TrackSearch } from './track-search.ts';

/** `ncPanelPushed` tells close to `back()` rather than `replace()`: `replace` never merges with the previous entry, and `back()` on a cold deep link would leave the app. */
declare module '@tanstack/history' {
  interface HistoryState {
    ncPanelPushed?: boolean;
    /** A plain Track selection resumes its saved viewport; explicit targets reveal themselves. */
    ncResumeTrackView?: boolean;
    ncFilePushed?: boolean;
    ncOpenPlanner?: boolean;
  }
}

export const PANEL_PUSHED_STATE_KEY = 'ncPanelPushed';
export const FILE_PUSHED_STATE_KEY = 'ncFilePushed';
export const PLANNER_OPEN_STATE_KEY = 'ncOpenPlanner';

export type NavTarget =
  | Readonly<{ name: 'today' }>
  /** Starting a track is a route, not a dialog; the track row is minted on submit, not on entering the route. */
  | Readonly<{ name: 'new-track'; areaId: string }>
  /** `blockId` rides in the hash; `cardId`/`filePath`/`panel`/`from` become `?card=`/`?file=`/`?panel=`/`?from=`. */
  | Readonly<{
    name: 'track';
    trackId: string;
    blockId?: string;
    cardId?: string;
    filePath?: string;
    panel?: MobilePanel;
    from?: TrackSource;
    openPlanner?: boolean;
  }>
  | Readonly<{ name: 'recipes' }>
  | Readonly<{ name: 'settings' }>
  | Readonly<{ name: 'settings-general' }>
  | Readonly<{ name: 'settings-network' }>
  /** Settings › Plugins — the installed list and its enable/disable switch. */
  | Readonly<{ name: 'settings-plugins' }>
  /** Settings › Planners — whether each Planner provider can run now (#1817). */
  | Readonly<{ name: 'settings-planners' }>
  | Readonly<{ name: 'settings-appearance' }>
  | Readonly<{ name: 'settings-about' }>;

export type GoOptions = Readonly<{ replace?: boolean }>;


export function pathFor(target: NavTarget): string {
  switch (target.name) {
    case 'today': return '/';
    case 'new-track': return `/area/${encodeURIComponent(target.areaId)}/new`;
    case 'track': return `/track/${encodeURIComponent(target.trackId)}`;
    case 'recipes': return '/recipes';
    case 'settings': return '/settings';
    case 'settings-general': return '/settings/general';
    case 'settings-network': return '/settings/network';
    case 'settings-plugins': return '/settings/plugins';
    case 'settings-planners': return '/settings/planners';
    case 'settings-appearance': return '/settings/appearance';
    case 'settings-about': return '/settings/about';
  }
}

/** Navigation is a `<button>` plus this callback, never `<a href>`: mixing the two forks Enter/Space activation semantics. */
export function useGo(): (target: NavTarget, options?: GoOptions) => void {
  const navigate = useNavigate();
  const router = useRouter();
  const views = useTrackViews();
  return useCallback((target: NavTarget, options?: GoOptions) => {
    // The block anchor rides in the hash: the track route remounts per track, so
    // state set before `go` would be discarded by the very move it describes.
    const hash = target.name === 'track' ? target.blockId : undefined;
    // All four fields are built from the target, never spread from the previous
    // location: "not passed" means "cleared".
    const search: TrackSearch = target.name === 'track'
      ? buildTrackSearch({ card: target.cardId, file: target.filePath, panel: target.panel, from: target.from })
      : {};
    // A plain cross-track selection resumes that track. Explicit links and
    // same-track commands retain their existing clear/replace semantics.
    const resumeTrack = target.name === 'track' && target.blockId === undefined
      && target.cardId === undefined && target.filePath === undefined
      && target.panel === undefined && target.openPlanner !== true
      && !router.state.location.pathname.endsWith(`/track/${encodeURIComponent(target.trackId)}`);
    if (resumeTrack) {
      Object.assign(search, views?.get(target.trackId)?.search);
    }
    // The planner-open intent rides on the history entry this navigation creates;
    // written only when asked for, so an ordinary move leaves `state` alone.
    const state = target.name === 'track' && target.openPlanner === true
      ? { [PLANNER_OPEN_STATE_KEY]: true }
      : undefined;
    void navigate({
      to: pathFor(target),
      hash,
      search,
      replace: options?.replace,
      ...(target.name === 'track' ? { state: { ...state, ncResumeTrackView: resumeTrack } } : {}),
    });
  }, [navigate, router, views]);
}

/**
 * The planner-open intent belongs to the one history entry the navigation
 * creates: it is consumed and struck off the first time that entry's route body
 * mounts, whether or not a planner card exists yet.
 */
export type PlannerOpenIntent = Readonly<{
  armed: boolean;
  disarm: () => void;
}>;

export function hasPlannerOpenMarker(state: unknown): boolean {
  if (typeof state !== 'object' || state === null) return false;
  return (state as Record<string, unknown>)[PLANNER_OPEN_STATE_KEY] === true;
}

export function usePlannerOpenIntent(trackId: string): PlannerOpenIntent {
  const navigate = useNavigate();
  const location = useRouterState({ select: (state) => state.location });
  const armed = hasPlannerOpenMarker(location.state)
    && routeParamFromPath(location.pathname, '/track/') === trackId;
  const disarm = useCallback(() => {
    void navigate({
      to: pathFor({ name: 'track', trackId }),
      // Everything else about this entry is kept: the disarm is not a
      // navigation the reader asked for, and it must be invisible to them.
      search: true,
      hash: true,
      replace: true,
      state: (previous) => {
        const next = { ...previous };
        delete next[PLANNER_OPEN_STATE_KEY];
        return next;
      },
    });
  }, [navigate, trackId]);
  return useMemo(() => ({ armed, disarm }), [armed, disarm]);
}

/** `card`, `file` and `panel` are mutually exclusive: a persisted card wins, then a transient file, then a panel. */
function buildTrackSearch(fields: Readonly<{
  card?: string; file?: string; panel?: MobilePanel; from?: TrackSource;
}>): TrackSearch {
  const search: { card?: string; file?: string; panel?: MobilePanel; from?: TrackSource } = {};
  if (fields.card !== undefined) search.card = fields.card;
  else if (fields.file !== undefined) search.file = fields.file;
  else if (fields.panel !== undefined) search.panel = fields.panel;
  if (fields.from !== undefined) search.from = fields.from;
  return search;
}

/** The card the current URL points at, or `null`. */
export function useRouteCardId(): string | null {
  return useRouterState({ select: (state) => cardIdFromLocation(state.location) });
}

/** The normalized workspace-relative file named by the current URL. */
export function useRouteFilePath(): string | null {
  return useRouterState({ select: (state) => filePathFromLocation(state.location) });
}

/** The shape every parser here reads; `ParsedLocation` satisfies it. */
export type SearchCarrier = Readonly<{
  searchStr?: string;
  search?: unknown;
  href?: string;
}>;

export function cardIdFromLocation(location: SearchCarrier): string | null {
  return rawParamFromLocation(location, 'card');
}

export function cardIdFromSearchString(searchStr: string): string | null {
  return rawParamFromSearchString(searchStr, 'card');
}

export function filePathFromSearchString(searchStr: string): string | null {
  const value = rawParamFromSearchString(searchStr, 'file');
  return value === null ? null : parseWorkspaceRelativeFilePath(value)?.path ?? null;
}

export function filePathFromLocation(location: SearchCarrier): string | null {
  const value = rawParamFromLocation(location, 'file');
  return value === null ? null : parseWorkspaceRelativeFilePath(value)?.path ?? null;
}

/** A value outside the union is dropped, not thrown on: the query string is user-editable text. */
export function panelFromSearchString(searchStr: string): MobilePanel | null {
  return asMobilePanel(rawParamFromSearchString(searchStr, 'panel'));
}

export function fromFromSearchString(searchStr: string): TrackSource | null {
  return asTrackSource(rawParamFromSearchString(searchStr, 'from'));
}

export function panelFromLocation(location: SearchCarrier): MobilePanel | null {
  return asMobilePanel(rawParamFromLocation(location, 'panel'));
}

export function fromFromLocation(location: SearchCarrier): TrackSource | null {
  return asTrackSource(rawParamFromLocation(location, 'from'));
}

export function asMobilePanel(value: string | null): MobilePanel | null {
  switch (value) {
    case 'outline': case 'cards': case 'tasks': case 'conversations': return value;
    default: return null;
  }
}

export function asTrackSource(value: string | null): TrackSource | null {
  switch (value) {
    case 'pages': case 'area': return value;
    default: return null;
  }
}

/** Reads the raw query string so a repeated key is rejected rather than folded by a parser. */
function rawParamFromLocation(location: SearchCarrier, key: string): string | null {
  if (typeof location.searchStr === 'string' && location.searchStr !== '') {
    return rawParamFromSearchString(location.searchStr, key);
  }
  if (typeof location.search === 'string') return rawParamFromSearchString(location.search, key);
  if (typeof location.href === 'string' && location.href.includes('?')) {
    const query = location.href.split('?')[1]?.split('#')[0] ?? '';
    const fromHref = rawParamFromSearchString(query, key);
    if (fromHref !== null) return fromHref;
    if (new URLSearchParams(query).getAll(key).length !== 1) return null;
  }
  if (typeof location.search === 'object' && location.search !== null) {
    const value = (location.search as Record<string, unknown>)[key];
    return typeof value === 'string' && value !== '' ? value : null;
  }
  return null;
}

function rawParamFromSearchString(searchStr: string, key: string): string | null {
  const raw = searchStr.startsWith('?') ? searchStr.slice(1) : searchStr;
  if (raw === '') return null;
  const values = new URLSearchParams(raw).getAll(key);
  if (values.length !== 1) return null;
  const value = values[0];
  return value === undefined || value === '' ? null : value;
}

/** The panel the current URL points at, or `null`. */
export function useRoutePanel(): MobilePanel | null {
  return useRouterState({ select: (state) => panelFromLocation(state.location) });
}

/** The surface the reader came from, or `null` (callers default to `pages`). */
export function useRouteFrom(): TrackSource | null {
  return useRouterState({ select: (state) => fromFromLocation(state.location) });
}

/** `validateSearch` for `/track/$trackId`; a repeated key arrives as an array and is dropped. */
export function validateTrackSearch(search: Record<string, unknown>): TrackSearch {
  const card = search.card;
  const file = search.file;
  const panel = search.panel;
  const from = search.from;
  return buildTrackSearch({
    card: typeof card === 'string' && card !== '' ? card : undefined,
    file: typeof file === 'string' ? parseWorkspaceRelativeFilePath(file)?.path : undefined,
    panel: asMobilePanel(typeof panel === 'string' ? panel : null) ?? undefined,
    from: asTrackSource(typeof from === 'string' ? from : null) ?? undefined,
  });
}

/**
 * The panel a renderer may open: `?panel=` only counts on a compact viewport
 * (above the breakpoint it would `inert` the desktop panel), and an open overlay
 * owns the surface.
 */
export function renderedMobilePanel(
  panel: MobilePanel | null,
  visit: Readonly<{ compact: boolean; overlayOpen: boolean }>,
): MobilePanel | null {
  if (!visit.compact || visit.overlayOpen) return null;
  return panel;
}

/** The block anchor the current URL points at, or `null`. */
export function useRouteHash(): string | null {
  const hash = useRouterState({ select: (state) => state.location.hash });
  return hash === '' ? null : hash;
}

export function useCurrentPath(): string {
  return useRouterState({ select: (state) => state.location.pathname });
}

/** Reads a path segment off the URL; `useParams({ strict: false })` widens to `any` outside a typed route context. */
export function useRouteParam(prefix: '/area/' | '/track/'): string | undefined {
  const path = useCurrentPath();
  return routeParamFromPath(path, prefix);
}

export function routeParamFromPath(path: string, prefix: '/area/' | '/track/'): string | undefined {
  if (!path.startsWith(prefix)) return undefined;
  const segment = path.slice(prefix.length).split('/', 1)[0];
  if (segment === '') return undefined;
  try {
    return decodeURIComponent(segment);
  } catch {
    return undefined;
  }
}

/**
 * The four fields exactly as they stand in `location`, deliberately not run
 * through `buildTrackSearch`: the card/panel exclusion is a rule about the URL a
 * navigation produces, not the one it reads.
 */
export function trackSearchFromLocation(location: SearchCarrier): TrackSearch {
  const search: { card?: string; file?: string; panel?: MobilePanel; from?: TrackSource } = {};
  const card = cardIdFromLocation(location);
  const file = filePathFromLocation(location);
  const panel = panelFromLocation(location);
  const from = fromFromLocation(location);
  if (card !== null) search.card = card;
  if (file !== null) search.file = file;
  if (panel !== null) search.panel = panel;
  if (from !== null) search.from = from;
  return search;
}

/** A key *present* with `undefined` clears that field; an absent key keeps what the URL holds (own-property, never value). */
export type TrackSearchPatch = Readonly<{
  card?: string | undefined;
  file?: string | undefined;
  panel?: MobilePanel | undefined;
  from?: TrackSource | undefined;
}>;

/**
 * The search `useGoSameTrack` would navigate to, or `null` when `location` is
 * not on `expectedTrackId`. Rebuilt field by field, never `{ ...prev, ...patch }`.
 */
export function sameTrackSearch(
  location: SearchCarrier & Readonly<{ pathname: string }>,
  expectedTrackId: string,
  patch: TrackSearchPatch,
): TrackSearch | null {
  if (routeParamFromPath(location.pathname, '/track/') !== expectedTrackId) return null;
  const current = trackSearchFromLocation(location);
  return buildTrackSearch({
    card: Object.hasOwn(patch, 'card') ? patch.card : current.card,
    file: Object.hasOwn(patch, 'file') ? patch.file : current.file,
    panel: Object.hasOwn(patch, 'panel') ? patch.panel : current.panel,
    from: Object.hasOwn(patch, 'from') ? patch.from : current.from,
  });
}

export type GoSameTrack = (
  expectedTrackId: string,
  patch: TrackSearchPatch,
  options?: GoOptions,
) => void;

/** The keeping exit: edit one whitelisted field of the current track's URL and leave the others, including the hash. */
export function useGoSameTrack(): GoSameTrack {
  const navigate = useNavigate();
  const go = useGo();
  const location = useRouterState({ select: (state) => state.location });
  return useCallback((expectedTrackId, patch, options) => {
    const search = sameTrackSearch(location, expectedTrackId, patch);
    if (search === null) {
      go(
        {
          name: 'track', trackId: expectedTrackId, cardId: patch.card,
          filePath: patch.file, panel: patch.panel, from: patch.from,
        },
        options,
      );
      return;
    }
    void navigate({
      to: pathFor({ name: 'track', trackId: expectedTrackId }),
      search,
      // `true` keeps the current value; `undefined` would clear it.
      hash: true,
      state: true,
      replace: options?.replace,
    });
  }, [go, location, navigate]);
}

export function hasPanelPushedMarker(state: unknown): boolean {
  if (typeof state !== 'object' || state === null) return false;
  return (state as Record<string, unknown>)[PANEL_PUSHED_STATE_KEY] === true;
}

export function hasFilePushedMarker(state: unknown): boolean {
  if (typeof state !== 'object' || state === null) return false;
  return (state as Record<string, unknown>)[FILE_PUSHED_STATE_KEY] === true;
}

export type TrackFileNavigation = Readonly<{
  openFile: (expectedTrackId: string, path: string) => void;
  closeFile: (expectedTrackId: string) => void;
}>;

/** The first file pushes a marked entry, file-to-file replaces it, close pops the marked entry; a cold deep link has no marker, so close replaces in place. */
export function useTrackFileNavigation(): TrackFileNavigation {
  const history = useRouter().history as RouterHistory;
  const navigate = useNavigate();
  const go = useGo();
  const location = useRouterState({ select: (state) => state.location });

  const openFile = useCallback((expectedTrackId: string, path: string) => {
    const search = sameTrackSearch(location, expectedTrackId, {
      file: path, card: undefined, panel: undefined,
    });
    if (search === null) {
      go({ name: 'track', trackId: expectedTrackId, filePath: path });
      return;
    }
    const switching = filePathFromLocation(location) !== null;
    void navigate({
      to: pathFor({ name: 'track', trackId: expectedTrackId }),
      search,
      hash: true,
      replace: switching,
      state: switching ? true : { ncFilePushed: true },
    });
  }, [go, location, navigate]);

  const closeFile = useCallback((expectedTrackId: string) => {
    if (hasFilePushedMarker(location.state) && history.canGoBack()) {
      history.back();
      return;
    }
    const search = sameTrackSearch(location, expectedTrackId, { file: undefined });
    if (search === null) {
      go({ name: 'track', trackId: expectedTrackId }, { replace: true });
      return;
    }
    void navigate({
      to: pathFor({ name: 'track', trackId: expectedTrackId }),
      search,
      hash: true,
      replace: true,
    });
  }, [go, history, location, navigate]);

  return { openFile, closeFile };
}

export type TrackPanelNavigation = Readonly<{
  openPanel: (expectedTrackId: string, panel: MobilePanel) => void;
  closePanel: (expectedTrackId: string) => void;
}>;

/**
 * Opening a panel pushes a marked entry, panel-to-panel replaces it, close pops
 * the marked entry; a cold `?panel=` deep link has no marker, so close replaces
 * (an unconditional `back()` would walk out of the app).
 */
export function useTrackPanelNavigation(): TrackPanelNavigation {
  // `useRouter()` is registered as `AnyRouter`, so its `history` widens to
  // `any`; naming the type back is what keeps `canGoBack`/`back` checked.
  const history = useRouter().history as RouterHistory;
  const navigate = useNavigate();
  const go = useGo();
  const location = useRouterState({ select: (state) => state.location });

  const openPanel = useCallback((expectedTrackId: string, panel: MobilePanel) => {
    const search = sameTrackSearch(location, expectedTrackId, {
      panel, card: undefined, file: undefined,
    });
    if (search === null) {
      go({ name: 'track', trackId: expectedTrackId, panel });
      return;
    }
    const switching = panelFromLocation(location) !== null;
    void navigate({
      to: pathFor({ name: 'track', trackId: expectedTrackId }),
      search,
      hash: true,
      replace: switching,
      state: switching ? true : { ncPanelPushed: true },
    });
  }, [go, location, navigate]);

  const closePanel = useCallback((expectedTrackId: string) => {
    if (hasPanelPushedMarker(location.state) && history.canGoBack()) {
      history.back();
      return;
    }
    const search = sameTrackSearch(location, expectedTrackId, { panel: undefined });
    if (search === null) {
      go({ name: 'track', trackId: expectedTrackId }, { replace: true });
      return;
    }
    void navigate({
      to: pathFor({ name: 'track', trackId: expectedTrackId }),
      search,
      hash: true,
      replace: true,
    });
  }, [go, history, location, navigate]);

  return { openPanel, closePanel };
}
