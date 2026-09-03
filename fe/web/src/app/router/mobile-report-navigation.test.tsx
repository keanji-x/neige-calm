// @vitest-environment jsdom
/*
 * #1191 §2 end to end: the real `AppShell`, the real track route, a real router
 * and a real memory history — no mocked `useGo`, no AppShell stand-in.
 *
 * That is the point. `responsive.contract.test.tsx` mocks
 * `@tanstack/react-router` and `navigation.ts` wholesale, so it can say nothing
 * about what lands in the URL, and `mobile.browser.test.tsx` drives a hand-built
 * copy of the shell. Everything this file asserts is a claim about how two
 * modules are *wired together* — the report's panel to `?panel=`, the report's
 * Back to `?from=` and the shell's sheets, the dock's visibility to the shell's
 * derived secondary flag — and a stub on either side would prove none of it.
 *
 * The pattern is `track-cards-panel.test.tsx` + `read-fallbacks.contract.test.tsx`:
 * `createAppRouter` + `router.update({ history: createMemoryHistory(...) })`.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const AREA = { id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
/* A second area with no tracks: the drill-in the shell must *replace*, not
   inherit, when a report hands it the area to return to. */
const OTHER_AREA = { id: 'c2', name: 'Second', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = {
  id: 'w1', area_id: 'c1', title: 'Responsive mobile UI', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};
const CARD = {
  id: 'card-term', track_id: 'w1', kind: 'terminal', title: 'Build log', sort: 1,
  payload: {}, deletable: true, created_at: 1, updated_at: 2,
};
/*
 * A report with one section and one task, which is what makes the Outline and
 * TASKS panels non-empty — both anchor landings (§1.4) go through the same
 * `openReportAnchor`, and neither had a URL assertion anywhere before.
 */
const REPORT_CARD = {
  id: 'card-report', track_id: 'w1', kind: 'track-report', title: 'Report card', sort: 2,
  deletable: false, created_at: 1, updated_at: 2,
  payload: {
    schemaVersion: 3,
    docRev: 1,
    summary: 's',
    body: 'b',
    blocks: [
      { id: 'b-1', kind: 'prose', rev: 1, payload: { markdown: '# Findings\n' } },
      {
        id: 'b-task', kind: 'task', rev: 1,
        payload: { key: 'ship-it', kind: 'terminal', declared_by: 'spec', ready: true, goal: 'g' },
      },
    ],
  },
};
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

function setup(path: string) {
  const transport: ApiTransportPort = {
    send(request) {
      if (request.path === '/api/areas') return Promise.resolve(ok([AREA, OTHER_AREA]));
      if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([TRACK]));
      if (request.path === '/api/areas/c2/tracks') return Promise.resolve(ok([]));
      if (request.path === '/api/tracks/w1') return Promise.resolve(ok({ track: TRACK, cards: [CARD, REPORT_CARD], overlays: [] }));
      if (request.path === '/api/tracks/w1/report') return Promise.resolve(ok({ taskDiagnostics: [] }));
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn(),
  });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return router;
}

const href = (router: ReturnType<typeof setup>) => router.state.location.href;
const dock = () => document.querySelector('nav[aria-label="Primary"]');
const mobilePanel = () => document.querySelector('[data-nc-mobile-page]');

/** The report's own three-dot menu — the control the focus contract returns to. */
const trackActions = () => screen.getByRole('button', { name: 'Track actions' });

/*
 * The dock is `inert` + `aria-hidden` whenever a secondary page is showing, so
 * a role query cannot see it — which is the contract working, not a hole. Its
 * buttons are addressed by their label instead, and every press below happens
 * in a state where the dock is genuinely visible and genuinely clickable.
 */
/*
 * Presses go through `userEvent`, not `fireEvent`: `fireEvent` dispatches a
 * click on a node whatever its state, so a dock that had wrongly stayed `inert`
 * would still "work" here. `userEvent` performs the press the way a reader
 * does. (`fireEvent.keyDown` on `document` below stays: Escape is a document
 * listener, not a control being pressed.)
 */
function dockButton(label: string): HTMLElement {
  const found = [...document.querySelectorAll<HTMLElement>('nav[aria-label="Primary"] button')]
    .find((button) => button.textContent === label);
  if (found === undefined) throw new Error(`no dock button labelled ${label}`);
  return found;
}

async function openPanelFromMenu(label: string): Promise<void> {
  await userEvent.click(await screen.findByRole('button', { name: 'Track actions' }));
  await userEvent.click(await screen.findByRole('menuitem', { name: label }));
}

/*
 * A `matchMedia` whose answer can *change*, with real listeners.
 *
 * The default stub in `beforeEach` reports compact and drops every listener on
 * the floor, which is fine for the tests that never leave the phone — but
 * widening the window is itself a reachable gesture (`?panel=` is a compact-only
 * concept), and a stub that cannot fire `change` cannot exercise it.
 */
function stubViewport(initiallyCompact: boolean) {
  const listeners = new Set<() => void>();
  let compact = initiallyCompact;
  vi.stubGlobal('matchMedia', vi.fn((media: string) => ({
    get matches() { return media.includes('width') ? compact : false; },
    media,
    onchange: null,
    // Only the width query's subscribers are replayed: `ThemeProvider` listens
    // to `prefers-color-scheme` through the same global and its handler reads
    // the event, which a synthetic width change does not have.
    addEventListener: (_type: string, listener: () => void) => {
      if (media.includes('width')) listeners.add(listener);
    },
    removeEventListener: (_type: string, listener: () => void) => { listeners.delete(listener); },
    addListener: vi.fn(), removeListener: vi.fn(), dispatchEvent: vi.fn(),
  })));
  return {
    widen() {
      compact = false;
      act(() => { for (const listener of [...listeners]) listener(); });
    },
  };
}

beforeEach(() => {
  // The compact branch of the shell is the subject; `RAIL_COLLAPSE_QUERY` is
  // the only media query it asks about.
  vi.stubGlobal('matchMedia', vi.fn((media: string) => ({
    matches: media.includes('width'), media, onchange: null,
    addEventListener: vi.fn(), removeEventListener: vi.fn(),
    addListener: vi.fn(), removeListener: vi.fn(), dispatchEvent: vi.fn(),
  })));
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
  Element.prototype.scrollIntoView = vi.fn();
});

afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe('the mobile report panel is the URL (#1191 §2.4)', () => {
  it('opens through ?panel= and puts focus in the panel container', async () => {
    const router = setup('/track/w1');
    await openPanelFromMenu('Cards');

    await waitFor(() => { expect(href(router)).toBe('/track/w1?panel=cards'); });
    expect(screen.getByRole('heading', { name: 'Cards' })).toBeTruthy();
    // §2.5 — the panel container takes focus, not whatever the menu left behind.
    await waitFor(() => { expect(document.activeElement).toBe(mobilePanel()); });
  });

  it('closes back to the report and returns focus to the three-dot menu', async () => {
    const router = setup('/track/w1');
    await openPanelFromMenu('Cards');
    await waitFor(() => { expect(href(router)).toBe('/track/w1?panel=cards'); });

    await userEvent.click(screen.getByRole('button', { name: 'Back to Report' }));
    await waitFor(() => { expect(href(router)).toBe('/track/w1'); });
    /*
     * The opener, not the document body. Closing removes the control the click
     * landed on, so without an explicit restore focus falls to `<body>` and a
     * keyboard reader has to Tab in from the top of the page again.
     */
    await waitFor(() => { expect(document.activeElement).toBe(trackActions()); });
  });

  it('lands focus in the panel on a cold-start deep link', async () => {
    setup('/track/w1?panel=tasks');
    expect(await screen.findByRole('heading', { name: 'Tasks' })).toBeTruthy();
    // Nobody clicked anything: the first render is already inside the panel.
    await waitFor(() => { expect(document.activeElement).toBe(mobilePanel()); });
  });

  it('answers the hardware Back button, focus included', async () => {
    const router = setup('/track/w1');
    await openPanelFromMenu('Cards');
    await waitFor(() => { expect(href(router)).toBe('/track/w1?panel=cards'); });

    // A POP, not a click — the panel state is nowhere but the URL, so this is
    // the whole of "the reader pressed Back".
    router.history.back();
    await waitFor(() => { expect(href(router)).toBe('/track/w1'); });
    expect(mobilePanel()?.getAttribute('data-nc-mobile-page')).toBe('closed');
    await waitFor(() => { expect(document.activeElement).toBe(trackActions()); });
  });

  it('takes the panel away when the reader walks off the report', async () => {
    const router = setup('/track/w1?panel=cards');
    expect(await screen.findByRole('heading', { name: 'Cards' })).toBeTruthy();

    await userEvent.click(await screen.findByRole('button', { name: 'Back to Pages' }));
    // The sheet is a different layer of the app; leaving the report layer drops
    // the report's panel (§2.1).
    await waitFor(() => { expect(href(router)).toBe('/track/w1'); });
    expect(screen.getByRole('dialog', { name: 'Pages' })).toBeTruthy();
  });

  /*
   * §0.3, on the *other* exit from a panel.
   *
   * `closePanel` earns its `back()` branch; the shell's "walking off the report"
   * exit used to be an unconditional `replace`, and `replace` does not merge
   * with the entry before it. Every open-then-leave cycle therefore left one
   * more `/track/w1` on the stack, and the reader had to press hardware Back
   * once per cycle to see anything change.
   *
   * The gesture is a real one and every press lands on a visible control: the
   * report's own Back button is inside `<main>`, which is only `inert` while a
   * sheet is open — the *dock* is what a panel-open report puts out of reach.
   * Escape closes the sheet the way a reader would, then the menu opens the
   * panel again.
   *
   * `router.history.length` is the whole point: the URL is identical at every
   * step, so nothing but the stack depth can tell the two behaviours apart.
   */
  it('does not stack a duplicate report entry each time the reader leaves the panel for a sheet', async () => {
    const router = setup('/track/w1');
    await screen.findByRole('button', { name: 'Track actions' });
    expect(router.history.length).toBe(1);

    for (let cycle = 0; cycle < 3; cycle += 1) {
      await openPanelFromMenu('Cards');
      await waitFor(() => { expect(href(router)).toBe('/track/w1?panel=cards'); });
      await userEvent.click(screen.getByRole('button', { name: 'Back to Pages' }));
      await waitFor(() => { expect(href(router)).toBe('/track/w1'); });
      expect(screen.getByRole('dialog', { name: 'Pages' })).toBeTruthy();
      fireEvent.keyDown(document, { key: 'Escape' });
      expect(screen.queryByRole('dialog', { name: 'Pages' })).toBeNull();
    }

    // One report entry and one panel entry, whatever the cycle count — the
    // `back()` branch pops the pushed panel instead of overwriting it.
    expect(router.history.length).toBe(2);
    // And the reader is standing on the report entry, so hardware Back leaves
    // the report rather than replaying three identical frames.
    expect(router.history.canGoBack()).toBe(false);
  });

  /*
   * §1.4's row for the Outline / TASKS anchor: `panel` cleared, `from` kept,
   * hash written. Both rows are the same `openReportAnchor`, and until now the
   * decision had *no* URL coverage anywhere — swapping it for a `goSameTrack`
   * that preserves `?panel=` left the whole suite green.
   */
  it('sends an outline entry to the block anchor and clears ?panel=', async () => {
    const router = setup('/track/w1?panel=outline&from=area');
    // Scoped to the sheet: the desktop report rail draws the very same outline,
    // and it is the mobile panel's copy whose landing is under test.
    const panel = await waitFor(() => { const found = mobilePanel(); expect(found).not.toBeNull(); return found!; });
    await userEvent.click(await within(panel as HTMLElement).findByRole('button', { name: /Findings/ }));
    await waitFor(() => { expect(href(router)).toBe('/track/w1?from=area#b-1-h1'); });
  });

  it('sends a TASKS entry to the block anchor and clears ?panel=', async () => {
    const router = setup('/track/w1?panel=tasks&from=area');
    const panel = await waitFor(() => { const found = mobilePanel(); expect(found).not.toBeNull(); return found!; });
    await userEvent.click(await within(panel as HTMLElement).findByRole('button', { name: /ship-it/ }));
    await waitFor(() => { expect(href(router)).toBe('/track/w1?from=area#b-task'); });
  });
});

describe('the shell derives whether a secondary page is showing (#1191 §2.1)', () => {
  it('hides the dock on the report, shows it over a sheet, and hides it again inside an area', async () => {
    setup('/track/w1?from=area');
    // On the report: the track route with no sheet open — the first OR branch.
    await waitFor(() => { expect(dock()?.getAttribute('aria-hidden')).toBe('true'); });

    // `?from=area` is the only thing that decides this label; there is no
    // stored report source any more (§1.2).
    await userEvent.click(await screen.findByRole('button', { name: 'Back to Tracks' }));
    expect(screen.getByRole('dialog', { name: 'Areas' })).toBeTruthy();
    // Restored straight into the track's own area, derived — never a stored id.
    expect(screen.getByRole('heading', { name: 'Product' })).toBeTruthy();

    /*
     * ── The §0.4 reference case ────────────────────────────────────────────
     * The pathname is still `/track/w1`, and the Areas sheet is drilled into a
     * area. The disproven ternary — `onTrackRoute ? section === null : …` —
     * returns `false` from its first branch here and never reaches the area
     * condition at all, so the dock reappears on top of a secondary page. Two
     * conditions OR'd is what keeps this hidden.
     */
    expect(dock()?.getAttribute('aria-hidden')).toBe('true');
  });

  /*
   * The whole reason the shell resets the drill-in on *entry* rather than at
   * every exit (#1191 §2.2). This is the reader's real gesture sequence, and
   * every click here lands on a control that is visible at the moment it is
   * pressed: closing the sheet leaves the selection behind — nothing reads it
   * while `mobileSection` is not `'areas'` — and pressing Areas again must not
   * reopen wherever they last were.
   */
  it('sends the dock’s Areas press to the area root list, never to the last drill-in', async () => {
    // Today, not a report: the track route is a secondary page on its own, and
    // this sequence has to be one a reader can perform with the dock in view.
    setup('/');
    await waitFor(() => dockButton('Areas'));
    await userEvent.click(dockButton('Areas'));
    screen.getByRole('dialog', { name: 'Areas' });
    await userEvent.click(await screen.findByRole('button', { name: /Product/ }));
    expect(screen.getByRole('heading', { name: 'Product' })).toBeTruthy();
    expect(dock()?.getAttribute('aria-hidden')).toBe('true');

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog', { name: 'Areas' })).toBeNull();
    // The sheet is closed and the drill-in is deliberately still remembered;
    // nothing reads it while the section is not Areas, so the dock is back.
    expect(dock()?.getAttribute('aria-hidden')).toBeNull();

    await userEvent.click(dockButton('Areas'));
    expect(screen.getByRole('heading', { name: 'Areas' })).toBeTruthy();
    expect(screen.queryByRole('heading', { name: 'Product' })).toBeNull();
  });

  it('defaults a report with no ?from= back to Pages', async () => {
    setup('/track/w1');
    await userEvent.click(await screen.findByRole('button', { name: 'Back to Pages' }));
    expect(screen.getByRole('dialog', { name: 'Pages' })).toBeTruthy();
  });

  it('replaces a remembered drill-in with the area the report returns to', async () => {
    setup('/');
    await waitFor(() => dockButton('Areas'));
    await userEvent.click(dockButton('Areas'));
    screen.getByRole('dialog', { name: 'Areas' });
    await userEvent.click(await screen.findByRole('button', { name: /Second/ }));
    expect(screen.getByRole('heading', { name: 'Second' })).toBeTruthy();
    fireEvent.keyDown(document, { key: 'Escape' });

    // A report reached from an area hands back its *own* area (§1.2, derived
    // from `track.areaId`), which has to win over whatever the sheet still held.
    await userEvent.click(dockButton('Areas'));
    await userEvent.click(await screen.findByRole('button', { name: /Product/ }));
    await userEvent.click(await screen.findByRole('button', { name: /Responsive mobile UI/ }));
    await userEvent.click(await screen.findByRole('button', { name: 'Back to Tracks' }));
    expect(screen.getByRole('heading', { name: 'Product' })).toBeTruthy();
    expect(screen.queryByRole('heading', { name: 'Second' })).toBeNull();
  });

  it('writes ?from= when a sheet is what opened the track', async () => {
    const router = setup('/');
    await waitFor(() => dockButton('Areas'));
    await userEvent.click(dockButton('Areas'));
    screen.getByRole('dialog', { name: 'Areas' });
    await userEvent.click(await screen.findByRole('button', { name: /Product/ }));
    await userEvent.click(await screen.findByRole('button', { name: /Responsive mobile UI/ }));
    await waitFor(() => { expect(href(router)).toBe('/track/w1?from=area'); });
    // The report is a secondary page whatever the closed sheet still remembers.
    expect(dock()?.getAttribute('aria-hidden')).toBe('true');
  });

});

/*
 * `?panel=` is a compact-only concept, and above the breakpoint it is not
 * harmless: `TrackPage` derives `mobilePanelOpen` from the prop alone and puts
 * `inert` + `aria-hidden` on the *desktop* panel surface, whose mobile
 * counterpart is `display: none` there. The result is a panel that is fully
 * visible and completely unreachable — the failure mode a11y tests exist for.
 */
describe('a desktop viewport never lets ?panel= disable the track panel', () => {
  it('keeps the desktop panel in the accessibility tree for a shared ?panel= link', async () => {
    stubViewport(false);
    const router = setup('/track/w1?panel=cards');

    // A role query is exactly the right instrument: `inert` + `aria-hidden`
    // take the surface out of the accessibility tree, so this heading — the
    // desktop CARDS module — disappears from it while the bug is present.
    expect(await screen.findByRole('heading', { name: 'Cards' })).toBeTruthy();
    /* `^` anchors the query to the row itself: the CARDS row now has a delete
       sibling whose accessible name also carries the card's title (#1231), and
       an unanchored match finds both. The row is what this case is about. */
    expect(await screen.findByRole('button', { name: /^Build log/ })).toBeTruthy();
    // And the URL stops claiming a state this viewport cannot be in.
    await waitFor(() => { expect(href(router)).toBe('/track/w1'); });
  });

  it('drops ?panel= when the reader widens the window with the panel open', async () => {
    const viewport = stubViewport(true);
    const router = setup('/track/w1?panel=cards');
    expect(await screen.findByRole('heading', { name: 'Cards' })).toBeTruthy();
    expect(href(router)).toBe('/track/w1?panel=cards');

    viewport.widen();

    // `replace`, not a push: widening a window is not a place to go Back to.
    await waitFor(() => { expect(href(router)).toBe('/track/w1'); });
    expect(router.history.length).toBe(1);
    /* `^` anchors the query to the row itself: the CARDS row now has a delete
       sibling whose accessible name also carries the card's title (#1231), and
       an unanchored match finds both. The row is what this case is about. */
    expect(await screen.findByRole('button', { name: /^Build log/ })).toBeTruthy();
  });
});
