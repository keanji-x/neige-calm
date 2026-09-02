// Route targets and the single navigation exit.

import { useNavigate, useRouter, useRouterState, type RouterHistory } from '@tanstack/react-router';
import { useCallback } from 'react';

/**
 * The mobile report's secondary panel, and the surface the reader reached the
 * report from. Both ride in the URL (#1191 §1) because they are navigation
 * *destinations*: a panel has to survive a reload, be shareable, and answer the
 * hardware Back button. Transient overlays (the Pages/Coves sheets, the mobile
 * card detail) deliberately stay in component state — see §0.1/§0.2.
 */
export type MobilePanel = 'outline' | 'cards' | 'tasks' | 'conversations';
export type WaveSource = 'pages' | 'cove';

/**
 * History state written by {@link useWavePanelNavigation} when opening a panel
 * pushed a new entry. Closing reads it back to decide between `back()` (no
 * duplicate entry) and `replace()` (cold-start deep link, where `back()` would
 * leave the app entirely). See #1191 §0.3 for the replace-only variant that was
 * disproven: `replace` never merges with the previous entry, so every
 * open/close cycle grew the stack by one silent duplicate.
 */
declare module '@tanstack/history' {
  interface HistoryState { ncPanelPushed?: boolean }
}

export const PANEL_PUSHED_STATE_KEY = 'ncPanelPushed';

export type NavTarget =
  | Readonly<{ name: 'today' }>
  | Readonly<{ name: 'cove'; coveId: string }>
  /**
   * `blockId` lands the reader on one block of the wave's report (§8.3).
   * `cardId` opens the card-grid overlay on that card (`?card=`).
   * `panel` opens the mobile report's secondary panel (`?panel=`), `from`
   * records the surface to return to (`?from=`).
   */
  | Readonly<{
    name: 'wave';
    waveId: string;
    blockId?: string;
    cardId?: string;
    panel?: MobilePanel;
    from?: WaveSource;
  }>
  | Readonly<{ name: 'settings' }>
  /**
   * #1230 — Settings drills in rather than stacking every group on one page.
   * `settings-templates` is the template list; `settings-template` is one
   * template's editor. Both are real routes and not page-local state, so Back
   * leaves the editor instead of leaving Settings, and a template's editor can
   * be linked to.
   */
  | Readonly<{ name: 'settings-templates' }>
  | Readonly<{ name: 'settings-template'; templateId: string }>;

export type GoOptions = Readonly<{ replace?: boolean }>;

/** The whitelisted wave query string, as the router validates and rebuilds it. */
export type WaveSearch = Readonly<{ card?: string; panel?: MobilePanel; from?: WaveSource }>;

export function pathFor(target: NavTarget): string {
  switch (target.name) {
    case 'today': return '/';
    case 'cove': return `/cove/${encodeURIComponent(target.coveId)}`;
    case 'wave': return `/wave/${encodeURIComponent(target.waveId)}`;
    case 'settings': return '/settings';
    case 'settings-templates': return '/settings/templates';
    case 'settings-template': return `/settings/templates/${encodeURIComponent(target.templateId)}`;
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
    // All three fields are constructed here, never spread from the previous
    // location: "not passed" means "cleared", for `panel` and `from` exactly as
    // it already meant for `card`. Callers that want a field kept must say so
    // (`useGoSameWave`, or by passing the value they read off the route).
    const search: WaveSearch = target.name === 'wave'
      ? buildWaveSearch({ card: target.cardId, panel: target.panel, from: target.from })
      : {};
    void navigate({ to: pathFor(target), hash, search, replace: options?.replace });
  }, [navigate]);
}

/**
 * `card` and `panel` are mutually exclusive.
 *
 * #1191 §0.1 proved the pair is self-contradictory rather than merely unusual:
 * a live `?card=` makes the card overlay open, and an open overlay force-closes
 * the mobile panel — so `?panel=cards&card=y` describes two states of one
 * surface. The card wins because it is the older, deep-linkable one.
 */
function buildWaveSearch(fields: Readonly<{
  card?: string; panel?: MobilePanel; from?: WaveSource;
}>): WaveSearch {
  const search: { card?: string; panel?: MobilePanel; from?: WaveSource } = {};
  if (fields.card !== undefined) search.card = fields.card;
  else if (fields.panel !== undefined) search.panel = fields.panel;
  if (fields.from !== undefined) search.from = fields.from;
  return search;
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

/**
 * `?panel=` and `?from=` are read through the *same* raw-query rule as `?card=`
 * — one occurrence, non-empty, off the unparsed string — so a repeated key
 * cannot mean one thing for one parameter and another for its neighbour. A
 * value outside the union is dropped rather than thrown on: the query string is
 * user-editable text, and a typo must degrade to "no panel", not to a crash.
 */
export function panelFromSearchString(searchStr: string): MobilePanel | null {
  return asMobilePanel(rawParamFromSearchString(searchStr, 'panel'));
}

export function fromFromSearchString(searchStr: string): WaveSource | null {
  return asWaveSource(rawParamFromSearchString(searchStr, 'from'));
}

export function panelFromLocation(location: SearchCarrier): MobilePanel | null {
  return asMobilePanel(rawParamFromLocation(location, 'panel'));
}

export function fromFromLocation(location: SearchCarrier): WaveSource | null {
  return asWaveSource(rawParamFromLocation(location, 'from'));
}

export function asMobilePanel(value: string | null): MobilePanel | null {
  switch (value) {
    case 'outline': case 'cards': case 'tasks': case 'conversations': return value;
    default: return null;
  }
}

export function asWaveSource(value: string | null): WaveSource | null {
  switch (value) {
    case 'pages': case 'cove': return value;
    default: return null;
  }
}

/**
 * Duplicates, empty values, and non-strings are rejected by reading the raw
 * query string (`getAll(key)`), not a parser that may have already folded
 * repeated keys.
 */
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
export function useRouteFrom(): WaveSource | null {
  return useRouterState({ select: (state) => fromFromLocation(state.location) });
}

/**
 * `validateSearch` for `/wave/$waveId`.
 *
 * TanStack hands over an already-parsed record, where a repeated key arrives as
 * an array — which the `typeof === 'string'` test drops, the same verdict the
 * raw-string parsers above reach. Living here rather than inline in the route
 * lets the tests drive the production validator instead of a copy of it.
 */
export function validateWaveSearch(search: Record<string, unknown>): WaveSearch {
  const card = search.card;
  const panel = search.panel;
  const from = search.from;
  return buildWaveSearch({
    card: typeof card === 'string' && card !== '' ? card : undefined,
    panel: asMobilePanel(typeof panel === 'string' ? panel : null) ?? undefined,
    from: asWaveSource(typeof from === 'string' ? from : null) ?? undefined,
  });
}

/**
 * The panel a *renderer* may open, given what else is true of this visit.
 *
 * `?panel=` is a compact-viewport concept. Above the breakpoint the mobile list
 * is `display: none`, but the surface still counts as open: `WavePage` derives
 * `mobilePanelOpen` from this value alone and puts `inert` + `aria-hidden` on
 * the *desktop* panel, so a shared `?panel=cards` link opened on a laptop drew
 * a panel that was fully visible and unreachable by keyboard or screen reader.
 *
 * It is a pure function, and exported, for one reason: the route also corrects
 * the URL in an effect, and an effect flushes inside `act` — so no jsdom test
 * can ever observe the frame this guard exists for. Here the decision is
 * directly assertable, and a mutant that drops the viewport term is red.
 *
 * `cardOpen` folds in the older exclusion (§0.1): a live `?card=` owns the
 * surface, so the panel is closed whatever the URL says.
 */
export function renderedMobilePanel(
  panel: MobilePanel | null,
  visit: Readonly<{ compact: boolean; cardOpen: boolean }>,
): MobilePanel | null {
  if (!visit.compact || visit.cardOpen) return null;
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

/**
 * Reads a single path segment straight off the URL.
 *
 * TanStack's `useParams({ strict: false })` widens to `any` outside a typed
 * route context, which the `no-unsafe-*` rules reject — and silencing them
 * would hand every route component an unchecked id. Parsing here keeps the id
 * a `string` and keeps the prefix table next to `pathFor`, so the two cannot
 * drift apart.
 */
export function useRouteParam(prefix: '/cove/' | '/wave/' | '/settings/templates/'): string | undefined {
  const path = useCurrentPath();
  return routeParamFromPath(path, prefix);
}

export function routeParamFromPath(path: string, prefix: '/cove/' | '/wave/' | '/settings/templates/'): string | undefined {
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
 * The whitelisted three fields **exactly as they stand in `location`** — each
 * one parsed and validated on its own, and deliberately *not* run through
 * `buildWaveSearch`.
 *
 * The card/panel exclusion is a rule about the URL a navigation *produces*, not
 * about the one it reads. Enforcing it here dropped `panel` on the way in, so a
 * patch that cleared `card` afterwards had nothing left to keep: the §1.4 row
 * for the illegal-`?card=` bounce ("panel kept, from kept, hash kept") lost the
 * panel whenever both were present, which is precisely the URL that bounce
 * exists to repair. The output of `sameWaveSearch` is still built by
 * `buildWaveSearch`, so the pair can never *leave* here together.
 */
export function waveSearchFromLocation(location: SearchCarrier): WaveSearch {
  const search: { card?: string; panel?: MobilePanel; from?: WaveSource } = {};
  const card = cardIdFromLocation(location);
  const panel = panelFromLocation(location);
  const from = fromFromLocation(location);
  if (card !== null) search.card = card;
  if (panel !== null) search.panel = panel;
  if (from !== null) search.from = from;
  return search;
}

/**
 * A patch for {@link useGoSameWave}. A key that is *present* with `undefined`
 * clears that field; a key that is absent keeps whatever the URL holds. The two
 * are distinguished by own-property, never by value.
 */
export type WaveSearchPatch = Readonly<{
  card?: string | undefined;
  panel?: MobilePanel | undefined;
  from?: WaveSource | undefined;
}>;

/**
 * The search `useGoSameWave` would navigate to, or `null` when `location` is
 * not on `expectedWaveId`.
 *
 * Rebuilt field by field from the *raw* location, never `{ ...prev, ...patch }`
 * (#1191 §1.3): spreading the previous search would carry unknown parameters,
 * arrays produced by repeated keys, and anything a future route adds across a
 * navigation this function is not supposed to be deciding about. The whitelist
 * is the point.
 */
export function sameWaveSearch(
  location: SearchCarrier & Readonly<{ pathname: string }>,
  expectedWaveId: string,
  patch: WaveSearchPatch,
): WaveSearch | null {
  if (routeParamFromPath(location.pathname, '/wave/') !== expectedWaveId) return null;
  const current = waveSearchFromLocation(location);
  return buildWaveSearch({
    card: Object.hasOwn(patch, 'card') ? patch.card : current.card,
    panel: Object.hasOwn(patch, 'panel') ? patch.panel : current.panel,
    from: Object.hasOwn(patch, 'from') ? patch.from : current.from,
  });
}

export type GoSameWave = (
  expectedWaveId: string,
  patch: WaveSearchPatch,
  options?: GoOptions,
) => void;

/**
 * The *keeping* exit: edit one whitelisted field of the current wave's URL and
 * leave the others — including the block anchor — where they are.
 *
 * It carries `expectedWaveId` rather than reading the route's own id so that
 * "am I still on that wave?" is a real, testable branch: a caller whose wave
 * has already changed under it falls back to {@link useGo}, which clears
 * everything, and a mutant that drops the check leaks the previous wave's
 * parameters onto the next one.
 */
export function useGoSameWave(): GoSameWave {
  const navigate = useNavigate();
  const go = useGo();
  const location = useRouterState({ select: (state) => state.location });
  return useCallback((expectedWaveId, patch, options) => {
    const search = sameWaveSearch(location, expectedWaveId, patch);
    if (search === null) {
      go(
        { name: 'wave', waveId: expectedWaveId, cardId: patch.card, panel: patch.panel, from: patch.from },
        options,
      );
      return;
    }
    void navigate({
      to: pathFor({ name: 'wave', waveId: expectedWaveId }),
      search,
      // `true` keeps the current value; `undefined` would clear it, which is
      // how the illegal-card bounce used to lose the reader's anchor.
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

export type WavePanelNavigation = Readonly<{
  openPanel: (expectedWaveId: string, panel: MobilePanel) => void;
  closePanel: (expectedWaveId: string) => void;
}>;

/**
 * The history strategy for the mobile report's panel (#1191 §1.1).
 *
 * | transition        | action                                              |
 * |-------------------|-----------------------------------------------------|
 * | report → panel    | `push`, marked `ncPanelPushed`                      |
 * | panel A → panel B | `replace`, marker preserved                         |
 * | panel → report    | marker **and** `canGoBack()` ⇒ `back()`; else `replace` |
 *
 * The `back()` branch is what keeps the stack flat; the `replace` branch is
 * what makes a shared `?panel=` deep link closable at all. An unconditional
 * `back()` on a cold start would walk out of the application.
 */
export function useWavePanelNavigation(): WavePanelNavigation {
  // `useRouter()` is registered as `AnyRouter`, so its `history` widens to
  // `any`; naming the type back is what keeps `canGoBack`/`back` checked.
  const history = useRouter().history as RouterHistory;
  const navigate = useNavigate();
  const go = useGo();
  const location = useRouterState({ select: (state) => state.location });

  const openPanel = useCallback((expectedWaveId: string, panel: MobilePanel) => {
    const search = sameWaveSearch(location, expectedWaveId, { panel, card: undefined });
    if (search === null) {
      go({ name: 'wave', waveId: expectedWaveId, panel });
      return;
    }
    const switching = panelFromLocation(location) !== null;
    void navigate({
      to: pathFor({ name: 'wave', waveId: expectedWaveId }),
      search,
      hash: true,
      replace: switching,
      // Spelled out rather than computed from `PANEL_PUSHED_STATE_KEY` so the
      // augmented `HistoryState` field is actually checked here.
      state: switching ? true : { ncPanelPushed: true },
    });
  }, [go, location, navigate]);

  const closePanel = useCallback((expectedWaveId: string) => {
    if (hasPanelPushedMarker(location.state) && history.canGoBack()) {
      history.back();
      return;
    }
    const search = sameWaveSearch(location, expectedWaveId, { panel: undefined });
    if (search === null) {
      go({ name: 'wave', waveId: expectedWaveId }, { replace: true });
      return;
    }
    void navigate({
      to: pathFor({ name: 'wave', waveId: expectedWaveId }),
      search,
      hash: true,
      replace: true,
    });
  }, [go, history, location, navigate]);

  return { openPanel, closePanel };
}
