// Route targets and the single navigation exit.

import { useNavigate, useRouter, useRouterState, type RouterHistory } from '@tanstack/react-router';
import { useCallback, useMemo } from 'react';

/**
 * The mobile report's secondary panel, and the surface the reader reached the
 * report from. Both ride in the URL (#1191 §1) because they are navigation
 * *destinations*: a panel has to survive a reload, be shareable, and answer the
 * hardware Back button. Transient overlays (the Pages/Areas sheets) deliberately
 * stay in component state — see §0.1/§0.2. (The mobile card detail page was the
 * other example here and is gone: #1234 S1b-4a removed it, because opening a
 * card is not offered on this viewport at all.)
 */
export type MobilePanel = 'outline' | 'cards' | 'tasks' | 'conversations';
export type TrackSource = 'pages' | 'area';

/**
 * History state written by {@link useTrackPanelNavigation} when opening a panel
 * pushed a new entry. Closing reads it back to decide between `back()` (no
 * duplicate entry) and `replace()` (cold-start deep link, where `back()` would
 * leave the app entirely). See #1191 §0.3 for the replace-only variant that was
 * disproven: `replace` never merges with the previous entry, so every
 * open/close cycle grew the stack by one silent duplicate.
 */
declare module '@tanstack/history' {
  interface HistoryState {
    ncPanelPushed?: boolean;
    /** See {@link useSpecOpenIntent}. */
    ncOpenSpec?: boolean;
  }
}

export const PANEL_PUSHED_STATE_KEY = 'ncPanelPushed';
export const SPEC_OPEN_STATE_KEY = 'ncOpenSpec';

export type NavTarget =
  | Readonly<{ name: 'today' }>
  | Readonly<{ name: 'area'; areaId: string }>
  /**
   * #1211 — starting a track is a **place**, not a dialog.
   *
   * The `+` used to open a modal over whatever you were looking at. It is now
   * a route: a page whose whole content is one composer, with the template and
   * the folder as chips under it. That is the same grammar as everywhere else
   * in the product — you say what you want — and unlike a modal it survives a
   * refresh, can be linked, and has a real Back.
   *
   * The track row is minted when the composer is submitted, not when this route
   * is entered. Two reasons, one of each kind: **product** — a `+` that mints a
   * row leaves an unnamed, intent-less track in the rail every time someone
   * changes their mind; and **mechanical** — picking a template *is* a fork of
   * that template's report inside the create transaction
   * (`routes/tracks.rs`), and `TrackPatch` carries no `template_id`, so a
   * template chosen after the row exists has nowhere to go. Minting late keeps
   * both choices where the kernel can still act on them.
   */
  | Readonly<{ name: 'new-track'; areaId: string }>
  /**
   * `blockId` lands the reader on one block of the track's report (§8.3).
   * `cardId` opens the card-grid overlay on that card (`?card=`).
   * `panel` opens the mobile report's secondary panel (`?panel=`), `from`
   * records the surface to return to (`?from=`).
   * `openSpec` asks the track being navigated *to* to open its spec
   * conversation on arrival — see {@link useSpecOpenIntent}.
   */
  | Readonly<{
    name: 'track';
    trackId: string;
    blockId?: string;
    cardId?: string;
    panel?: MobilePanel;
    from?: TrackSource;
    openSpec?: boolean;
  }>
  | Readonly<{ name: 'settings' }>
  /**
   * #1230 — Settings drills in rather than stacking every group on one page.
   * Each group is a real route, so Back leaves the group instead of leaving
   * Settings and a group can be linked to. (#1230's own two entries,
   * `settings-templates` and `settings-template`, are gone with the template
   * editor — #1300 S1.)
   */
  /** Settings › Plugins — the installed list and its enable/disable switch. */
  | Readonly<{ name: 'settings-plugins' }>
  /**
   * Appearance and About are sections of their own rather than blocks stacked
   * on one page: a settings group with its own heading is a nav entry, and a
   * pane holding three of them is the pile that shape produced.
   */
  | Readonly<{ name: 'settings-appearance' }>
  | Readonly<{ name: 'settings-about' }>;

export type GoOptions = Readonly<{ replace?: boolean }>;

/** The whitelisted track query string, as the router validates and rebuilds it. */
export type TrackSearch = Readonly<{ card?: string; panel?: MobilePanel; from?: TrackSource }>;

export function pathFor(target: NavTarget): string {
  switch (target.name) {
    case 'today': return '/';
    case 'area': return `/area/${encodeURIComponent(target.areaId)}`;
    case 'new-track': return `/area/${encodeURIComponent(target.areaId)}/new`;
    case 'track': return `/track/${encodeURIComponent(target.trackId)}`;
    case 'settings': return '/settings';
    case 'settings-plugins': return '/settings/plugins';
    case 'settings-appearance': return '/settings/appearance';
    case 'settings-about': return '/settings/about';
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
    // because it has to survive the navigation that carries it: the track route
    // remounts per track, so state set before `go` would be discarded by the
    // very move it was describing. A hash also makes the deep link real —
    // pasting one lands on the same paragraph.
    const hash = target.name === 'track' ? target.blockId : undefined;
    // Track search is always explicit so a card query cannot leak onto today
    // or a different track, and so clearing `cardId` actually drops `?card=`.
    // All three fields are constructed here, never spread from the previous
    // location: "not passed" means "cleared", for `panel` and `from` exactly as
    // it already meant for `card`. Callers that want a field kept must say so
    // (`useGoSameTrack`, or by passing the value they read off the route).
    const search: TrackSearch = target.name === 'track'
      ? buildTrackSearch({ card: target.cardId, panel: target.panel, from: target.from })
      : {};
    // The spec-open intent rides on the history entry this navigation creates,
    // and nowhere else (#1211 S2) — see `useSpecOpenIntent`. Written only when
    // asked for, so an ordinary move leaves `state` alone.
    const state = target.name === 'track' && target.openSpec === true
      ? { [SPEC_OPEN_STATE_KEY]: true }
      : undefined;
    void navigate({
      to: pathFor(target),
      hash,
      search,
      replace: options?.replace,
      ...(state === undefined ? {} : { state }),
    });
  }, [navigate]);
}

/**
 * "Open the spec conversation of the track this navigation is going to."
 *
 * ── Why this is a property of the *navigation* and not a slot somewhere ─────
 *
 * The track a create answers with has no name and nothing said in it, and the
 * spec card's id does not exist yet — `POST /api/tracks` answers with a `Track`,
 * and the card arrives a route later with the track detail. So the shell cannot
 * say "open card X"; it can only say "open the spec conversation of track W",
 * and something on the other side of the navigation has to redeem that.
 *
 * The first shape put it in a provider above the outlet, as one global
 * `requestedSpecTrackId`, and the review found it broken from both ends at
 * once. Whoever redeems it must also clear it, and no single component can own
 * that: a route body that clears what it cannot redeem takes the intent away
 * from the track it was meant for (the rail's `+` is on screen on every route,
 * so the track being left is still mounted when the intent is stated), while a
 * route body that only clears its own leaves a stale intent standing when the
 * landing never happens — the detail read failing is enough — and it springs
 * the drawer open on some unrelated visit later. Tightening either end loosens
 * the other, because the slot has no owner.
 *
 * A history entry does have one. The intent is written into the state of the
 * entry the navigation creates, so:
 *
 *  - only the route body rendering *that* location ever sees it — no other
 *    body can redeem it, and none can clear it either;
 *  - redemption `disarm()`s by replacing the entry without the marker, so once
 *    that body has run for it — whether or not there was a card to open — the
 *    entry is an ordinary one for good.
 *
 * ── What the intent is attached to, stated so it can be checked ─────────────
 *
 * The intent belongs to **that one history entry**. It is consumed the first
 * time the entry's route body mounts at all — that is, the first time the track
 * detail lands — and struck off unconditionally at that moment; if the detail
 * in hand has a spec card, the same pass opens it. So "no spec card" is not a
 * reason to keep the mark: the entry has been displayed, the create has been
 * answered as well as it can be, and a card that arrives on a later read of the
 * same entry finds nothing armed (`track-untitled.test.tsx` pins exactly that).
 * The intent is *not* scoped to a span of the reader's attention either: it
 * does not expire because they walked away, because walking away only means
 * they are standing on a different entry.
 *
 * So an entry that never displayed its body still carries the mark. The
 * reachable case is the failed landing — the detail read errors, `TrackRoute`
 * renders the error box, and the body that consumes the mark never mounts —
 * and that entry arms again when it is displayed again, whether a reload, the Retry
 * button, or the Back button. The first two are the point: the create asked for
 * this conversation and the landing finally worked. Back reaches the same entry
 * and gets the same answer, which is what owning the intent per-entry costs.
 * A *fresh* navigation to the same track is a different entry and carries no
 * mark, so an ordinary later visit stays ordinary. Both directions are pinned
 * in `track-untitled.test.tsx`.
 *
 * `armed` is additionally gated on the location naming `trackId`. That is a belt
 * over the braces and not the mechanism — what keeps one track from redeeming
 * another's mark is that the mark lives on an entry only one route body renders
 * — and in the app it cannot be observed false: `TrackRoute` builds the body
 * only once the detail in hand is that track's, and renders nothing in between.
 * It stays because this hook is exported and takes its track id from the caller,
 * and a caller asking about a track the location does not name should be told
 * `false` rather than handed somebody else's marker.
 */
export type SpecOpenIntent = Readonly<{ armed: boolean; disarm: () => void }>;

export function hasSpecOpenMarker(state: unknown): boolean {
  if (typeof state !== 'object' || state === null) return false;
  return (state as Record<string, unknown>)[SPEC_OPEN_STATE_KEY] === true;
}

export function useSpecOpenIntent(trackId: string): SpecOpenIntent {
  const navigate = useNavigate();
  const location = useRouterState({ select: (state) => state.location });
  const armed = hasSpecOpenMarker(location.state)
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
        delete next[SPEC_OPEN_STATE_KEY];
        return next;
      },
    });
  }, [navigate, trackId]);
  return useMemo(() => ({ armed, disarm }), [armed, disarm]);
}

/**
 * `card` and `panel` are mutually exclusive.
 *
 * #1191 §0.1 proved the pair is self-contradictory rather than merely unusual:
 * a live `?card=` makes the card overlay open, and an open overlay force-closes
 * the mobile panel — so `?panel=cards&card=y` describes two states of one
 * surface. The card wins because it is the older, deep-linkable one.
 */
function buildTrackSearch(fields: Readonly<{
  card?: string; panel?: MobilePanel; from?: TrackSource;
}>): TrackSearch {
  const search: { card?: string; panel?: MobilePanel; from?: TrackSource } = {};
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
export function useRouteFrom(): TrackSource | null {
  return useRouterState({ select: (state) => fromFromLocation(state.location) });
}

/**
 * `validateSearch` for `/track/$trackId`.
 *
 * TanStack hands over an already-parsed record, where a repeated key arrives as
 * an array — which the `typeof === 'string'` test drops, the same verdict the
 * raw-string parsers above reach. Living here rather than inline in the route
 * lets the tests drive the production validator instead of a copy of it.
 */
export function validateTrackSearch(search: Record<string, unknown>): TrackSearch {
  const card = search.card;
  const panel = search.panel;
  const from = search.from;
  return buildTrackSearch({
    card: typeof card === 'string' && card !== '' ? card : undefined,
    panel: asMobilePanel(typeof panel === 'string' ? panel : null) ?? undefined,
    from: asTrackSource(typeof from === 'string' ? from : null) ?? undefined,
  });
}

/**
 * The panel a *renderer* may open, given what else is true of this visit.
 *
 * `?panel=` is a compact-viewport concept. Above the breakpoint the mobile list
 * is `display: none`, but the surface still counts as open: `TrackPage` derives
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
 * The whitelisted three fields **exactly as they stand in `location`** — each
 * one parsed and validated on its own, and deliberately *not* run through
 * `buildTrackSearch`.
 *
 * The card/panel exclusion is a rule about the URL a navigation *produces*, not
 * about the one it reads. Enforcing it here dropped `panel` on the way in, so a
 * patch that cleared `card` afterwards had nothing left to keep: the §1.4 row
 * for the illegal-`?card=` bounce ("panel kept, from kept, hash kept") lost the
 * panel whenever both were present, which is precisely the URL that bounce
 * exists to repair. The output of `sameTrackSearch` is still built by
 * `buildTrackSearch`, so the pair can never *leave* here together.
 */
export function trackSearchFromLocation(location: SearchCarrier): TrackSearch {
  const search: { card?: string; panel?: MobilePanel; from?: TrackSource } = {};
  const card = cardIdFromLocation(location);
  const panel = panelFromLocation(location);
  const from = fromFromLocation(location);
  if (card !== null) search.card = card;
  if (panel !== null) search.panel = panel;
  if (from !== null) search.from = from;
  return search;
}

/**
 * A patch for {@link useGoSameTrack}. A key that is *present* with `undefined`
 * clears that field; a key that is absent keeps whatever the URL holds. The two
 * are distinguished by own-property, never by value.
 */
export type TrackSearchPatch = Readonly<{
  card?: string | undefined;
  panel?: MobilePanel | undefined;
  from?: TrackSource | undefined;
}>;

/**
 * The search `useGoSameTrack` would navigate to, or `null` when `location` is
 * not on `expectedTrackId`.
 *
 * Rebuilt field by field from the *raw* location, never `{ ...prev, ...patch }`
 * (#1191 §1.3): spreading the previous search would carry unknown parameters,
 * arrays produced by repeated keys, and anything a future route adds across a
 * navigation this function is not supposed to be deciding about. The whitelist
 * is the point.
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
    panel: Object.hasOwn(patch, 'panel') ? patch.panel : current.panel,
    from: Object.hasOwn(patch, 'from') ? patch.from : current.from,
  });
}

export type GoSameTrack = (
  expectedTrackId: string,
  patch: TrackSearchPatch,
  options?: GoOptions,
) => void;

/**
 * The *keeping* exit: edit one whitelisted field of the current track's URL and
 * leave the others — including the block anchor — where they are.
 *
 * It carries `expectedTrackId` rather than reading the route's own id so that
 * "am I still on that track?" is a real, testable branch: a caller whose track
 * has already changed under it falls back to {@link useGo}, which clears
 * everything, and a mutant that drops the check leaks the previous track's
 * parameters onto the next one.
 */
export function useGoSameTrack(): GoSameTrack {
  const navigate = useNavigate();
  const go = useGo();
  const location = useRouterState({ select: (state) => state.location });
  return useCallback((expectedTrackId, patch, options) => {
    const search = sameTrackSearch(location, expectedTrackId, patch);
    if (search === null) {
      go(
        { name: 'track', trackId: expectedTrackId, cardId: patch.card, panel: patch.panel, from: patch.from },
        options,
      );
      return;
    }
    void navigate({
      to: pathFor({ name: 'track', trackId: expectedTrackId }),
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

export type TrackPanelNavigation = Readonly<{
  openPanel: (expectedTrackId: string, panel: MobilePanel) => void;
  closePanel: (expectedTrackId: string) => void;
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
export function useTrackPanelNavigation(): TrackPanelNavigation {
  // `useRouter()` is registered as `AnyRouter`, so its `history` widens to
  // `any`; naming the type back is what keeps `canGoBack`/`back` checked.
  const history = useRouter().history as RouterHistory;
  const navigate = useNavigate();
  const go = useGo();
  const location = useRouterState({ select: (state) => state.location });

  const openPanel = useCallback((expectedTrackId: string, panel: MobilePanel) => {
    const search = sameTrackSearch(location, expectedTrackId, { panel, card: undefined });
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
      // Spelled out rather than computed from `PANEL_PUSHED_STATE_KEY` so the
      // augmented `HistoryState` field is actually checked here.
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
