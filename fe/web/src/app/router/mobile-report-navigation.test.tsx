// @vitest-environment jsdom
/*
 * #1191 §2 end to end: the real `AppShell`, the real wave route, a real router
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
 * The pattern is `wave-cards-panel.test.tsx` + `read-fallbacks.contract.test.tsx`:
 * `createAppRouter` + `router.update({ history: createMemoryHistory(...) })`.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const COVE = { id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
/* A second cove with no waves: the drill-in the shell must *replace*, not
   inherit, when a report hands it the cove to return to. */
const OTHER_COVE = { id: 'c2', name: 'Second', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };
const WAVE = {
  id: 'w1', cove_id: 'c1', title: 'Responsive mobile UI', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};
const CARD = {
  id: 'card-term', wave_id: 'w1', kind: 'terminal', title: 'Build log', sort: 1,
  payload: {}, deletable: true, created_at: 1, updated_at: 2,
};
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });

function setup(path: string) {
  const transport: ApiTransportPort = {
    send(request) {
      if (request.path === '/api/coves') return Promise.resolve(ok([COVE, OTHER_COVE]));
      if (request.path === '/api/coves/c1/waves') return Promise.resolve(ok([WAVE]));
      if (request.path === '/api/coves/c2/waves') return Promise.resolve(ok([]));
      if (request.path === '/api/waves/w1') return Promise.resolve(ok({ wave: WAVE, cards: [CARD], overlays: [] }));
      if (request.path === '/api/waves/w1/report') return Promise.resolve(ok({ taskDiagnostics: [] }));
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
const waveActions = () => screen.getByRole('button', { name: 'Wave actions' });

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
  await userEvent.click(await screen.findByRole('button', { name: 'Wave actions' }));
  await userEvent.click(await screen.findByRole('menuitem', { name: label }));
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
    const router = setup('/wave/w1');
    await openPanelFromMenu('Cards');

    await waitFor(() => { expect(href(router)).toBe('/wave/w1?panel=cards'); });
    expect(screen.getByRole('heading', { name: 'Cards' })).toBeTruthy();
    // §2.5 — the panel container takes focus, not whatever the menu left behind.
    await waitFor(() => { expect(document.activeElement).toBe(mobilePanel()); });
  });

  it('closes back to the report and returns focus to the three-dot menu', async () => {
    const router = setup('/wave/w1');
    await openPanelFromMenu('Cards');
    await waitFor(() => { expect(href(router)).toBe('/wave/w1?panel=cards'); });

    await userEvent.click(screen.getByRole('button', { name: 'Back to Report' }));
    await waitFor(() => { expect(href(router)).toBe('/wave/w1'); });
    /*
     * The opener, not the document body. Closing removes the control the click
     * landed on, so without an explicit restore focus falls to `<body>` and a
     * keyboard reader has to Tab in from the top of the page again.
     */
    await waitFor(() => { expect(document.activeElement).toBe(waveActions()); });
  });

  it('lands focus in the panel on a cold-start deep link', async () => {
    setup('/wave/w1?panel=tasks');
    expect(await screen.findByRole('heading', { name: 'Tasks' })).toBeTruthy();
    // Nobody clicked anything: the first render is already inside the panel.
    await waitFor(() => { expect(document.activeElement).toBe(mobilePanel()); });
  });

  it('answers the hardware Back button, focus included', async () => {
    const router = setup('/wave/w1');
    await openPanelFromMenu('Cards');
    await waitFor(() => { expect(href(router)).toBe('/wave/w1?panel=cards'); });

    // A POP, not a click — the panel state is nowhere but the URL, so this is
    // the whole of "the reader pressed Back".
    router.history.back();
    await waitFor(() => { expect(href(router)).toBe('/wave/w1'); });
    expect(mobilePanel()?.getAttribute('data-nc-mobile-page')).toBe('closed');
    await waitFor(() => { expect(document.activeElement).toBe(waveActions()); });
  });

  it('takes the panel away when the reader walks off the report', async () => {
    const router = setup('/wave/w1?panel=cards');
    expect(await screen.findByRole('heading', { name: 'Cards' })).toBeTruthy();

    await userEvent.click(await screen.findByRole('button', { name: 'Back to Pages' }));
    // The sheet is a different layer of the app; leaving the report layer drops
    // the report's panel (§2.1).
    await waitFor(() => { expect(href(router)).toBe('/wave/w1'); });
    expect(screen.getByRole('dialog', { name: 'Pages' })).toBeTruthy();
  });
});

describe('the shell derives whether a secondary page is showing (#1191 §2.1)', () => {
  it('hides the dock on the report, shows it over a sheet, and hides it again inside a cove', async () => {
    setup('/wave/w1?from=cove');
    // On the report: the wave route with no sheet open — the first OR branch.
    await waitFor(() => { expect(dock()?.getAttribute('aria-hidden')).toBe('true'); });

    // `?from=cove` is the only thing that decides this label; there is no
    // stored report source any more (§1.2).
    await userEvent.click(await screen.findByRole('button', { name: 'Back to Waves' }));
    expect(screen.getByRole('dialog', { name: 'Coves' })).toBeTruthy();
    // Restored straight into the wave's own cove, derived — never a stored id.
    expect(screen.getByRole('heading', { name: 'Product' })).toBeTruthy();

    /*
     * ── The §0.4 reference case ────────────────────────────────────────────
     * The pathname is still `/wave/w1`, and the Coves sheet is drilled into a
     * cove. The disproven ternary — `onWaveRoute ? section === null : …` —
     * returns `false` from its first branch here and never reaches the cove
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
   * while `mobileSection` is not `'coves'` — and pressing Coves again must not
   * reopen wherever they last were.
   */
  it('sends the dock’s Coves press to the cove root list, never to the last drill-in', async () => {
    // Today, not a report: the wave route is a secondary page on its own, and
    // this sequence has to be one a reader can perform with the dock in view.
    setup('/');
    await waitFor(() => dockButton('Coves'));
    await userEvent.click(dockButton('Coves'));
    screen.getByRole('dialog', { name: 'Coves' });
    await userEvent.click(await screen.findByRole('button', { name: /Product/ }));
    expect(screen.getByRole('heading', { name: 'Product' })).toBeTruthy();
    expect(dock()?.getAttribute('aria-hidden')).toBe('true');

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog', { name: 'Coves' })).toBeNull();
    // The sheet is closed and the drill-in is deliberately still remembered;
    // nothing reads it while the section is not Coves, so the dock is back.
    expect(dock()?.getAttribute('aria-hidden')).toBeNull();

    await userEvent.click(dockButton('Coves'));
    expect(screen.getByRole('heading', { name: 'Coves' })).toBeTruthy();
    expect(screen.queryByRole('heading', { name: 'Product' })).toBeNull();
  });

  it('defaults a report with no ?from= back to Pages', async () => {
    setup('/wave/w1');
    await userEvent.click(await screen.findByRole('button', { name: 'Back to Pages' }));
    expect(screen.getByRole('dialog', { name: 'Pages' })).toBeTruthy();
  });

  it('replaces a remembered drill-in with the cove the report returns to', async () => {
    setup('/');
    await waitFor(() => dockButton('Coves'));
    await userEvent.click(dockButton('Coves'));
    screen.getByRole('dialog', { name: 'Coves' });
    await userEvent.click(await screen.findByRole('button', { name: /Second/ }));
    expect(screen.getByRole('heading', { name: 'Second' })).toBeTruthy();
    fireEvent.keyDown(document, { key: 'Escape' });

    // A report reached from a cove hands back its *own* cove (§1.2, derived
    // from `wave.coveId`), which has to win over whatever the sheet still held.
    await userEvent.click(dockButton('Coves'));
    await userEvent.click(await screen.findByRole('button', { name: /Product/ }));
    await userEvent.click(await screen.findByRole('button', { name: /Responsive mobile UI/ }));
    await userEvent.click(await screen.findByRole('button', { name: 'Back to Waves' }));
    expect(screen.getByRole('heading', { name: 'Product' })).toBeTruthy();
    expect(screen.queryByRole('heading', { name: 'Second' })).toBeNull();
  });

  it('writes ?from= when a sheet is what opened the wave', async () => {
    const router = setup('/');
    await waitFor(() => dockButton('Coves'));
    await userEvent.click(dockButton('Coves'));
    screen.getByRole('dialog', { name: 'Coves' });
    await userEvent.click(await screen.findByRole('button', { name: /Product/ }));
    await userEvent.click(await screen.findByRole('button', { name: /Responsive mobile UI/ }));
    await waitFor(() => { expect(href(router)).toBe('/wave/w1?from=cove'); });
    // The report is a secondary page whatever the closed sheet still remembers.
    expect(dock()?.getAttribute('aria-hidden')).toBe('true');
  });

});
