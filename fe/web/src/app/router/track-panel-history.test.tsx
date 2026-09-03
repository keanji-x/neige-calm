// @vitest-environment jsdom
/*
 * The mobile panel's history strategy (#1191 §1.1), driven through a real
 * router and a real memory history — never a mocked `useGo`. `responsive.
 * contract.test.tsx` mocks `@tanstack/react-router` wholesale, which is why it
 * cannot say anything about pushes, replaces, or the Back button; the pattern
 * copied here is `track-cards-panel.test.tsx` / `read-fallbacks.contract.test.tsx`
 * instead. The route below is a stand-in for `/track/$trackId` only in its
 * *component*: its `validateSearch` is the production `validateTrackSearch`, so
 * a broken validator fails here.
 *
 * History assertions read `router.history.length`. `window.history.length` is
 * pinned at 1 under jsdom and would pass no matter what this code did.
 */
import {
  Outlet, RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter,
} from '@tanstack/react-router';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  useGoSameTrack, useRouteCardId, useRouteFrom, useRouteHash, useRoutePanel, useRouteParam,
  useTrackPanelNavigation, validateTrackSearch, type TrackSearch,
} from './navigation.ts';

function Probe() {
  const trackId = useRouteParam('/track/') ?? '';
  const { openPanel, closePanel } = useTrackPanelNavigation();
  const goSameTrack = useGoSameTrack();
  const panel = useRoutePanel();
  const from = useRouteFrom();
  const card = useRouteCardId();
  const hash = useRouteHash();
  return (
    <div>
      <span>{`panel:${panel ?? 'none'}`}</span>
      <span>{`from:${from ?? 'none'}`}</span>
      <span>{`card:${card ?? 'none'}`}</span>
      <span>{`hash:${hash ?? 'none'}`}</span>
      <button type="button" onClick={() => { openPanel(trackId, 'cards'); }}>open cards</button>
      <button type="button" onClick={() => { openPanel(trackId, 'tasks'); }}>open tasks</button>
      <button type="button" onClick={() => { closePanel(trackId); }}>close panel</button>
      <button type="button" onClick={() => { goSameTrack(trackId, { card: undefined }, { replace: true }); }}>drop card</button>
      <button type="button" onClick={() => { goSameTrack('w9', { card: undefined }); }}>drop card on w9</button>
      <button type="button" onClick={() => { goSameTrack(trackId, { from: undefined }); }}>drop from</button>
    </div>
  );
}

function setup(initialEntries: readonly string[]) {
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const todayRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/',
    component: () => <span>today</span>,
  });
  const trackRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: '/track/$trackId',
    validateSearch: (search: Record<string, unknown>): TrackSearch => validateTrackSearch(search),
    component: Probe,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([todayRoute, trackRoute]),
    history: createMemoryHistory({ initialEntries: [...initialEntries] }),
  });
  render(<RouterProvider router={router} />);
  return router;
}

const href = (router: ReturnType<typeof setup>) => router.state.location.href;
/* Probe values are read as text, not through a dynamic DOM query. */
const probe = (name: string, value: string) => screen.getByText(`${name}:${value}`);

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe('opening the mobile panel', () => {
  it('pushes one entry carrying the marker', async () => {
    const router = setup(['/', '/track/w1']);
    await screen.findByText('open cards');
    expect(router.history.length).toBe(2);

    await userEvent.click(screen.getByText('open cards'));
    await waitFor(() => { probe('panel', 'cards'); });
    expect(href(router)).toBe('/track/w1?panel=cards');
    expect(router.history.length).toBe(3);
    expect(router.history.location.state.ncPanelPushed).toBe(true);
  });

  it('replaces when swapping panels, and keeps the marker', async () => {
    const router = setup(['/', '/track/w1']);
    await userEvent.click(await screen.findByText('open cards'));
    await waitFor(() => { probe('panel', 'cards'); });

    await userEvent.click(screen.getByText('open tasks'));
    await waitFor(() => { probe('panel', 'tasks'); });
    // One entry for the whole visit to the panel layer, not one per panel.
    expect(router.history.length).toBe(3);
    expect(router.history.location.state.ncPanelPushed).toBe(true);
  });

  it('ignores an illegal value and lets the route validator drop foreign ones', async () => {
    const router = setup(['/track/w1?panel=bogus&debug=1']);
    // The parser refuses the illegal value rather than throwing on it.
    await waitFor(() => { probe('panel', 'none'); });

    await userEvent.click(screen.getByText('open cards'));
    /*
     * Two facts in one URL, both owned by `validateTrackSearch` (which
     * `buildLocation` runs on the way out): `panel=cards` is recognised and
     * kept, and `debug=1` is not on the whitelist and is gone. Strip the
     * `panel` branch from the validator and this href loses the panel.
     */
    await waitFor(() => { expect(href(router)).toBe('/track/w1?panel=cards'); });
  });

  it('keeps the return surface and the block anchor', async () => {
    const router = setup(['/track/w1?from=area#b3']);
    await userEvent.click(await screen.findByText('open cards'));
    await waitFor(() => { probe('panel', 'cards'); });
    expect(href(router)).toBe('/track/w1?panel=cards&from=area#b3');
  });
});

describe('closing the mobile panel', () => {
  it('[#1191 §0.3] pops the pushed entry instead of stacking a duplicate', async () => {
    const router = setup(['/', '/track/w1']);
    await userEvent.click(await screen.findByText('open cards'));
    await waitFor(() => { expect(href(router)).toBe('/track/w1?panel=cards'); });

    await userEvent.click(screen.getByText('close panel'));
    await waitFor(() => { expect(href(router)).toBe('/track/w1'); });

    /*
     * The decisive step. A `replace`-only close leaves `[/, /track/w1,
     * /track/w1]`, so this Back would land on the track again and the reader
     * would have to press twice to leave — the silent growth §0.3 records.
     * Stepping back must reach Today.
     */
    router.history.back();
    await waitFor(() => { expect(router.state.location.pathname).toBe('/'); });
    expect(await screen.findByText('today')).toBeTruthy();
  });

  it('survives three open/close cycles without growing the stack', async () => {
    const router = setup(['/', '/track/w1']);
    for (let cycle = 0; cycle < 3; cycle += 1) {
      await userEvent.click(await screen.findByText('open cards'));
      await waitFor(() => { probe('panel', 'cards'); });
      await userEvent.click(screen.getByText('close panel'));
      await waitFor(() => { probe('panel', 'none'); });
    }
    router.history.back();
    await waitFor(() => { expect(router.state.location.pathname).toBe('/'); });
  });

  it('replaces on a cold-start deep link, and never steps out of the app', async () => {
    const router = setup(['/track/w1?panel=cards&from=pages']);
    await waitFor(() => { probe('panel', 'cards'); });
    expect(router.history.canGoBack()).toBe(false);
    const back = vi.spyOn(router.history, 'back');

    await userEvent.click(screen.getByText('close panel'));
    await waitFor(() => { expect(href(router)).toBe('/track/w1?from=pages'); });
    // An unconditional `back()` here would leave the application entirely.
    expect(back).not.toHaveBeenCalled();
    expect(router.history.length).toBe(1);
    expect(router.state.location.pathname).toBe('/track/w1');
  });
});

describe('useGoSameTrack', () => {
  it('keeps the panel, the return surface and the anchor while dropping the card', async () => {
    const router = setup(['/track/w1?card=c1&from=area#b7']);
    await waitFor(() => { probe('card', 'c1'); });

    await userEvent.click(screen.getByText('drop card'));
    await waitFor(() => { expect(href(router)).toBe('/track/w1?from=area#b7'); });
    expect(router.history.length).toBe(1);
  });

  it('clears everything when the track it was given is not the track in the URL', async () => {
    const router = setup(['/track/w1?card=c1&from=area#b7']);
    await waitFor(() => { probe('card', 'c1'); });

    // `w9` is not this URL's track, so this is an ordinary navigation and no
    // parameter of `w1` may ride along.
    await userEvent.click(screen.getByText('drop card on w9'));
    await waitFor(() => { expect(router.state.location.pathname).toBe('/track/w9'); });
    expect(href(router)).toBe('/track/w9');
  });

  it('distinguishes an explicit clear from a field it was not asked about', async () => {
    const router = setup(['/track/w1?panel=tasks&from=area']);
    await waitFor(() => { probe('from', 'area'); });

    await userEvent.click(screen.getByText('drop from'));
    await waitFor(() => { expect(href(router)).toBe('/track/w1?panel=tasks'); });
  });
});
