// @vitest-environment jsdom
/* The real `AppShell`, the real track route, a real router and a real memory history: every claim
 * here is about how two modules are wired together, which a stub on either side could not prove. */
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
/* A second empty Area makes the centered selector’s destination observable. */
const OTHER_AREA = { id: 'c2', name: 'Second', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = {
  id: 'w1', area_id: 'c1', title: 'Responsive mobile UI', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};
const CARD = {
  id: 'card-term', track_id: 'w1', kind: 'terminal', title: 'Build log', sort: 1,
  payload: {}, deletable: true, created_at: 1, updated_at: 2,
};
/* One section and one task, which is what makes the Outline and TASKS panels non-empty. */
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
        payload: { key: 'ship-it', kind: 'terminal', declared_by: 'spec', ready: true, command: 'true' },
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
      if (request.path === '/api/tracks/w1') return Promise.resolve(ok({
        track: TRACK, can_resume: false, cards: [CARD, REPORT_CARD], overlays: [],
      }));
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
const mobilePanel = () => document.querySelector('[data-nc-mobile-page]');

const trackActions = () => screen.getByRole('button', { name: 'Track actions' });

async function openPanelFromMenu(label: string): Promise<void> {
  await userEvent.click(await screen.findByRole('button', { name: 'Track actions' }));
  await userEvent.click(await screen.findByRole('menuitem', { name: label }));
}

/* A `matchMedia` whose answer can change, with real listeners: widening the window is a reachable gesture. */
function stubViewport(initiallyCompact: boolean) {
  const listeners = new Set<() => void>();
  let compact = initiallyCompact;
  vi.stubGlobal('matchMedia', vi.fn((media: string) => ({
    get matches() { return media.includes('width') ? compact : false; },
    media,
    onchange: null,
    // Only the width query's subscribers are replayed: `ThemeProvider`'s handler reads the event, which
    // a synthetic width change does not have.
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
  // `RAIL_COLLAPSE_QUERY` is the only media query the compact shell asks about.
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
    // The panel container takes focus, not whatever the menu left behind.
    await waitFor(() => { expect(document.activeElement).toBe(mobilePanel()); });
  });

  it('closes back to the report and returns focus to the three-dot menu', async () => {
    const router = setup('/track/w1');
    await openPanelFromMenu('Cards');
    await waitFor(() => { expect(href(router)).toBe('/track/w1?panel=cards'); });

    await userEvent.click(screen.getByRole('button', { name: 'Back to Report' }));
    await waitFor(() => { expect(href(router)).toBe('/track/w1'); });
    /* The opener, not the body: closing removes the control the click landed on. */
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

    // A POP, not a click: the panel state is nowhere but the URL.
    router.history.back();
    await waitFor(() => { expect(href(router)).toBe('/track/w1'); });
    expect(mobilePanel()?.getAttribute('data-nc-mobile-page')).toBe('closed');
    await waitFor(() => { expect(document.activeElement).toBe(trackActions()); });
  });

  it('takes the panel away when the reader walks off the report', async () => {
    const router = setup('/track/w1?panel=cards');
    expect(await screen.findByRole('heading', { name: 'Cards' })).toBeTruthy();

    await userEvent.click(await screen.findByRole('button', { name: 'Open areas' }));
    // Leaving the report layer drops the report's panel.
    await waitFor(() => { expect(href(router)).toBe('/track/w1'); });
    expect(screen.getByRole('dialog', { name: 'Tracks and settings' })).toBeTruthy();
  });

  /* `closePanel`'s `back()` branch: an unconditional `replace` does not merge with the entry before it,
   * so every open-then-leave cycle would stack one more `/track/w1`. The URL is identical at every
   * step, so only `router.history.length` can tell the two behaviours apart. */
  it('does not stack a duplicate report entry each time the reader leaves the panel for a sheet', async () => {
    const router = setup('/track/w1');
    await screen.findByRole('button', { name: 'Track actions' });
    expect(router.history.length).toBe(1);

    for (let cycle = 0; cycle < 3; cycle += 1) {
      await openPanelFromMenu('Cards');
      await waitFor(() => { expect(href(router)).toBe('/track/w1?panel=cards'); });
      await userEvent.click(screen.getByRole('button', { name: 'Open areas' }));
      await waitFor(() => { expect(href(router)).toBe('/track/w1'); });
      expect(screen.getByRole('dialog', { name: 'Tracks and settings' })).toBeTruthy();
      fireEvent.keyDown(document, { key: 'Escape' });
      expect(screen.queryByRole('dialog', { name: 'Tracks and settings' })).toBeNull();
    }

    // One report entry and one panel entry, whatever the cycle count.
    expect(router.history.length).toBe(2);
    expect(router.history.canGoBack()).toBe(false);
  });

  /* `panel` cleared, `from` kept, hash written; a `goSameTrack` that preserved `?panel=` would pass every other case. */
  it('sends an outline entry to the block anchor and clears ?panel=', async () => {
    const router = setup('/track/w1?panel=outline&from=area');
    // Scoped to the sheet: the desktop report rail draws the very same outline.
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

describe('workspace header navigation', () => {
  it('opens the current track’s area without a dock', async () => {
    setup('/track/w1?from=area');
    await userEvent.click(await screen.findByRole('button', { name: 'Open areas' }));
    expect(screen.getByRole('dialog', { name: 'Tracks and settings' })).toBeTruthy();
    expect(screen.getByRole('heading', { name: 'Areas' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Product' }));
    expect(screen.getByRole('heading', { name: 'Product' })).toBeTruthy();
    expect(document.querySelector('nav[aria-label="Primary"]')).toBeNull();
  });

  it('navigates between Areas through the main centered menu and opens that Area’s track list', async () => {
    const router = setup('/area/c1/new');
    await userEvent.click(await screen.findByRole('button', { name: 'Switch area, Product' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Second' }));
    await waitFor(() => { expect(href(router)).toBe('/area/c2/new'); });
    await userEvent.click(screen.getByRole('button', { name: 'Open areas' }));
    await userEvent.click(screen.getByRole('button', { name: 'Second' }));
    expect(screen.getByRole('heading', { name: 'Second' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Back to Areas' })).toBeTruthy();
    expect(screen.queryByRole('heading', { name: 'Areas' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: /^New track$/ }));
    await waitFor(() => { expect(href(router)).toBe('/area/c2/new'); });
    expect(screen.getByRole('button', { name: 'Switch area, Second' })).toBeTruthy();
  });

  it('writes ?from= when navigation opens the track', async () => {
    const router = setup('/');
    await userEvent.click(await screen.findByRole('button', { name: 'Open areas' }));
    await userEvent.click(screen.getByRole('button', { name: 'Product' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Responsive mobile UI' }));
    await waitFor(() => { expect(href(router)).toBe('/track/w1?from=area'); });
    expect(document.querySelector('nav[aria-label="Primary"]')).toBeNull();
  });

  it('opens the first Area composer at home and switches Area from the centered selector', async () => {
    const router = setup('/');
    await waitFor(() => { expect(href(router)).toBe('/area/c1/new'); });
    await userEvent.click(screen.getByRole('button', { name: 'Switch area, Product' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Second' }));
    await waitFor(() => { expect(href(router)).toBe('/area/c2/new'); });
    expect(screen.getByRole('button', { name: 'Switch area, Second' })).toBeTruthy();
  });
});

/* Above the breakpoint `?panel=` is not harmless: `TrackPage` derives `mobilePanelOpen` from the prop
 * alone and puts `inert` + `aria-hidden` on the DESKTOP panel surface — visible and unreachable. */
describe('a desktop viewport never lets ?panel= disable the track panel', () => {
  it('keeps the desktop panel in the accessibility tree for a shared ?panel= link', async () => {
    stubViewport(false);
    const router = setup('/track/w1?panel=cards');

    // A role query is the right instrument: `inert` + `aria-hidden` take the surface out of the accessibility tree.
    expect(await screen.findByRole('heading', { name: 'Cards' })).toBeTruthy();
    /* `^` anchors to the row itself: the delete sibling's accessible name also carries the card's title. */
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
    /* `^` anchors to the row itself: the delete sibling's accessible name also carries the card's title. */
    expect(await screen.findByRole('button', { name: /^Build log/ })).toBeTruthy();
  });
});
