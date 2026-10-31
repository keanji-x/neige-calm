import type { ParsedLocation } from '@tanstack/react-router';
import { createElement, useLayoutEffect } from 'react';
import { act, cleanup, waitFor } from '@testing-library/react';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import { useDraftBackOwner, type NavigationSource } from './draft-back-owner.tsx';
import { useGo, useNewTrackBack } from './navigation.ts';
import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  cardIdFromLocation, cardIdFromSearchString, filePathFromLocation, filePathFromSearchString,
  fromFromLocation, fromFromSearchString,
  hasFilePushedMarker, hasPanelPushedMarker, hasPlannerOpenMarker,
  panelFromLocation, panelFromSearchString, pathFor,
  renderedMobilePanel, sameTrackSearch, validateTrackSearch,
} from './navigation.ts';

describe('cardIdFromSearchString', () => {
  it('reads a single card query', () => {
    expect(cardIdFromSearchString('?card=term-1')).toBe('term-1');
    expect(cardIdFromSearchString('card=term-1')).toBe('term-1');
  });

  it('rejects empty, missing, and duplicate card queries', () => {
    expect(cardIdFromSearchString('')).toBeNull();
    expect(cardIdFromSearchString('?other=1')).toBeNull();
    expect(cardIdFromSearchString('?card=')).toBeNull();
    expect(cardIdFromSearchString('?card=a&card=b')).toBeNull();
  });
});

describe('cardIdFromLocation', () => {
  it('prefers the raw search string so duplicates are visible', () => {
    expect(cardIdFromLocation({ searchStr: '?card=a&card=b', search: { card: 'a' } })).toBeNull();
    expect(cardIdFromLocation({ searchStr: '?card=term-1' })).toBe('term-1');
  });
});

describe('filePathFromSearchString', () => {
  it('reads exactly one non-empty file query and rejects duplicates', () => {
    expect(filePathFromSearchString('?file=src%2Fmain.rs')).toBe('src/main.rs');
    expect(filePathFromSearchString('?file=docs%2F100%2520done.md')).toBe('docs/100%20done.md');
    expect(filePathFromSearchString('?file=')).toBeNull();
    expect(filePathFromSearchString('?file=a&file=b')).toBeNull();
    expect(filePathFromLocation({ searchStr: '?file=README.md' })).toBe('README.md');
  });
});

describe('pathFor', () => {
  it('gives the mobile Network category a dedicated URL', () => {
    expect(pathFor({ name: 'settings-network' })).toBe('/settings/network');
    expect(pathFor({ name: 'settings' })).toBe('/settings');
  });
  it('does not embed the card query in the path', () => {
    expect(pathFor({ name: 'track', trackId: 'w1', cardId: 'c1' })).toBe('/track/w1');
  });
});

describe('panel and from parsers', () => {
  it('reads a single legal value', () => {
    expect(panelFromSearchString('?panel=cards')).toBe('cards');
    expect(panelFromSearchString('panel=outline&from=area')).toBe('outline');
    expect(fromFromSearchString('?from=pages')).toBe('pages');
    expect(fromFromSearchString('?from=area')).toBe('area');
  });

  it('drops empty, missing, illegal, and duplicated values without throwing', () => {
    expect(panelFromSearchString('')).toBeNull();
    expect(panelFromSearchString('?panel=')).toBeNull();
    expect(panelFromSearchString('?other=1')).toBeNull();
    // Illegal: the query string is user-editable text, so a typo degrades.
    expect(panelFromSearchString('?panel=Cards')).toBeNull();
    expect(panelFromSearchString('?panel=report')).toBeNull();
    // The same duplicate-key verdict `?card=` reaches, not a folded first-wins.
    expect(panelFromSearchString('?panel=cards&panel=tasks')).toBeNull();
    expect(panelFromSearchString('?panel=cards&panel=cards')).toBeNull();
    expect(fromFromSearchString('')).toBeNull();
    expect(fromFromSearchString('?from=')).toBeNull();
    expect(fromFromSearchString('?from=today')).toBeNull();
    expect(fromFromSearchString('?from=pages&from=area')).toBeNull();
  });

  it('reads the raw string in preference to an already-folded object', () => {
    expect(panelFromLocation({ searchStr: '?panel=cards&panel=tasks', search: { panel: 'cards' } })).toBeNull();
    expect(fromFromLocation({ searchStr: '?from=pages&from=area', search: { from: 'pages' } })).toBeNull();
    expect(panelFromLocation({ searchStr: '?panel=tasks' })).toBe('tasks');
    expect(fromFromLocation({ search: { from: 'area' } })).toBe('area');
  });
});

describe('validateTrackSearch', () => {
  it('keeps the four whitelisted fields and drops everything else', () => {
    expect(validateTrackSearch({ file: 'src/main.rs', from: 'area', other: 'x' }))
      .toEqual({ file: 'src/main.rs', from: 'area' });
  });

  it('drops illegal values, empty strings, and repeated keys parsed as arrays', () => {
    expect(validateTrackSearch({ panel: 'report', from: 'today', card: '' })).toEqual({});
    expect(validateTrackSearch({ panel: ['cards', 'tasks'], from: ['pages'] })).toEqual({});
    expect(validateTrackSearch({})).toEqual({});
  });

  it('[#1191 §0.1] drops the panel when a card is present', () => {
    expect(validateTrackSearch({ card: 'c1', panel: 'cards', from: 'pages' }))
      .toEqual({ card: 'c1', from: 'pages' });
  });

  it('keeps only one primary report surface, with card then file precedence', () => {
    expect(validateTrackSearch({ file: 'README.md', panel: 'tasks', from: 'pages' }))
      .toEqual({ file: 'README.md', from: 'pages' });
    expect(validateTrackSearch({ card: 'c1', file: 'README.md', panel: 'cards' }))
      .toEqual({ card: 'c1' });
  });
});

describe('sameTrackSearch', () => {
  const onW1 = { pathname: '/track/w1', searchStr: '?card=c1&from=area' };

  it('refuses a location that is not on the expected track', () => {
    expect(sameTrackSearch(onW1, 'w2', { card: undefined })).toBeNull();
    expect(sameTrackSearch({ pathname: '/', searchStr: '' }, 'w1', {})).toBeNull();
    expect(sameTrackSearch({ pathname: '/area/w1', searchStr: '' }, 'w1', {})).toBeNull();
  });

  it('keeps the fields the patch does not mention', () => {
    expect(sameTrackSearch(onW1, 'w1', {})).toEqual({ card: 'c1', from: 'area' });
    expect(sameTrackSearch(onW1, 'w1', { card: undefined })).toEqual({ from: 'area' });
  });

  it('opens a file without carrying a card or panel and clears only that file on close', () => {
    const panel = { pathname: '/track/w1', searchStr: '?panel=tasks&from=area' };
    expect(sameTrackSearch(panel, 'w1', { file: 'src/main.rs' }))
      .toEqual({ file: 'src/main.rs', from: 'area' });
    const file = { pathname: '/track/w1', searchStr: '?file=src%2Fmain.rs&from=area' };
    expect(sameTrackSearch(file, 'w1', { file: undefined })).toEqual({ from: 'area' });
  });

  it('distinguishes an absent key from an explicit undefined', () => {
    const location = { pathname: '/track/w1', searchStr: '?panel=tasks&from=pages' };
    expect(sameTrackSearch(location, 'w1', { from: undefined })).toEqual({ panel: 'tasks' });
    expect(sameTrackSearch(location, 'w1', {})).toEqual({ panel: 'tasks', from: 'pages' });
  });

  it('rebuilds from the whitelist instead of spreading the previous search', () => {
    const polluted = {
      pathname: '/track/w1',
      searchStr: '?panel=tasks&debug=1&card=a&card=b&from=area',
    };
    // `debug` never crosses; the duplicated `card` is rejected rather than
    // carried through as an array.
    expect(sameTrackSearch(polluted, 'w1', {})).toEqual({ panel: 'tasks', from: 'area' });
  });

  it('[#1191 §0.1] clears the panel whenever a card is set', () => {
    const location = { pathname: '/track/w1', searchStr: '?panel=cards&from=pages' };
    expect(sameTrackSearch(location, 'w1', { card: 'c9' })).toEqual({ card: 'c9', from: 'pages' });
  });

  /* The card/panel exclusion belongs to the search this builds, not the one it reads: normalising
   * `?card=bad&panel=tasks` on read would leave the bounce's `{ card: undefined }` no panel to keep. */
  it('[#1191 §1.4] keeps the panel when the illegal-card bounce clears the card', () => {
    const location = { pathname: '/track/w1', searchStr: '?card=bad&panel=tasks&from=area' };
    expect(sameTrackSearch(location, 'w1', { card: undefined })).toEqual({ panel: 'tasks', from: 'area' });
    // Reading alone still never emits the pair: the patch is what releases it.
    expect(sameTrackSearch(location, 'w1', {})).toEqual({ card: 'bad', from: 'area' });
  });
});

describe('hasPanelPushedMarker', () => {
  it('accepts only the exact marker', () => {
    expect(hasPanelPushedMarker({ ncPanelPushed: true })).toBe(true);
    expect(hasPanelPushedMarker({ ncPanelPushed: false })).toBe(false);
    expect(hasPanelPushedMarker({ ncPanelPushed: 'true' })).toBe(false);
    expect(hasPanelPushedMarker({})).toBe(false);
    expect(hasPanelPushedMarker(null)).toBe(false);
    expect(hasPanelPushedMarker(undefined)).toBe(false);
  });
});

describe('hasFilePushedMarker', () => {
  it('accepts only the exact marker', () => {
    expect(hasFilePushedMarker({ ncFilePushed: true })).toBe(true);
    expect(hasFilePushedMarker({ ncFilePushed: false })).toBe(false);
    expect(hasFilePushedMarker({ ncFilePushed: 'true' })).toBe(false);
    expect(hasFilePushedMarker({})).toBe(false);
    expect(hasFilePushedMarker(null)).toBe(false);
  });
});

/* History state is whatever an older app version or a hand-edited entry left behind, so the marker is
 * read as an exact `true`, never as truthiness. */
describe('hasPlannerOpenMarker', () => {
  it('accepts only the exact marker', () => {
    expect(hasPlannerOpenMarker({ ncOpenPlanner: true })).toBe(true);
    expect(hasPlannerOpenMarker({ ncOpenPlanner: false })).toBe(false);
    expect(hasPlannerOpenMarker({ ncOpenPlanner: 'true' })).toBe(false);
    expect(hasPlannerOpenMarker({})).toBe(false);
    expect(hasPlannerOpenMarker(null)).toBe(false);
    expect(hasPlannerOpenMarker(undefined)).toBe(false);
  });
});

describe('renderedMobilePanel', () => {
  /* The route also clears `?panel=` in an effect that flushes inside `act`, so jsdom cannot see the
   * frame a real browser paints; the guard is asserted where it is decidable. */
  it('[#1191] refuses to open a panel above the compact breakpoint', () => {
    expect(renderedMobilePanel('cards', { compact: false, overlayOpen: false })).toBeNull();
    expect(renderedMobilePanel('cards', { compact: true, overlayOpen: false })).toBe('cards');
  });

  it('[#1191 §0.1] yields the surface to a live card overlay', () => {
    expect(renderedMobilePanel('tasks', { compact: true, overlayOpen: true })).toBeNull();
    expect(renderedMobilePanel(null, { compact: true, overlayOpen: false })).toBeNull();
  });
});


type NavigationPort = { go: ReturnType<typeof useGo>; back: ReturnType<typeof useNewTrackBack>; owner: NonNullable<ReturnType<typeof useDraftBackOwner>> };
function NavigationProbe({ ready }: { ready: (port: NavigationPort) => void }) {
  const go = useGo();
  const back = useNewTrackBack();
  const owner = useDraftBackOwner()!;
  useLayoutEffect(() => { ready({ go, back, owner }); }, [go, back, owner, ready]);
  return null;
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); });
async function pendingDraftBack() {
  let port: NavigationPort | undefined;
  const fixture = renderDailyFixture({ initial: '/track/project?from=pages#source', routeProbe: createElement(NavigationProbe, { ready: next => { port = next; } }) });
  const { router } = fixture;
  await waitFor(() => { expect(port).toBeDefined(); expect(router.state.status).toBe('idle'); });
  act(() => { port!.go({ name: 'new-track', areaId: 'project-area' }); });
  await waitFor(() => { expect(port!.back).toBeTypeOf('function'); expect(router.state.status).toBe('idle'); });
  const release = router.history.back.bind(router.history);
  const request = vi.spyOn(router.history, 'back').mockImplementation(() => {});
  act(() => { port!.back!(); });
  return { ...fixture, port: () => port!, release, request };
}

describe('draft Back supersession owner', () => {
  it('waits for actual delivery, consumes once before reentry, and suppresses duplicate Back', async () => {
    const { router, port, release, request } = await pendingDraftBack();
    const old = vi.fn();
    const reentry = vi.fn();
    const latest = vi.fn<(source: NavigationSource) => void>(() => { port().owner.go(reentry); });
    act(() => {
      port().owner.go(old);
      port().owner.go(latest);
      port().back!();
      // An idle origin and an unrelated resolved event are not a Back ack.
      router.emit({ type: 'onResolved', toLocation: router.state.location as ParsedLocation, fromLocation: router.state.location as ParsedLocation, pathChanged: false, hrefChanged: false, hashChanged: false });
      port().owner.ready();
    });
    expect(latest).not.toHaveBeenCalled();
    expect(request).toHaveBeenCalledExactlyOnceWith();
    act(() => { release(); });
    await waitFor(() => { expect(latest).toHaveBeenCalledTimes(1); });
    expect(latest.mock.calls[0]?.[0]?.href).toBe('/track/project?from=pages#source');
    expect(old).not.toHaveBeenCalled();
    expect(reentry).toHaveBeenCalledTimes(1);
    act(() => { port().owner.ready(); });
    expect(latest).toHaveBeenCalledTimes(1);
  });

  it.each(['settings', 'today', 'track'] as const)('shares latest-wins across useGo targets: %s', async (name) => {
    const { router, port, release, requests } = await pendingDraftBack();
    act(() => {
      port().go({ name: 'new-track', areaId: 'project-area' });
      port().go(name === 'track' ? { name, trackId: 'project', from: 'area', blockId: 'latest' } : { name });
      release();
    });
    const href = name === 'settings' ? '/settings' : name === 'today' ? '/' : '/track/project?from=area#latest';
    await waitFor(() => { expect(router.state.location.href).toBe(href); expect(router.state.status).toBe('idle'); });
    expect(requests.filter(request => request.method !== 'GET')).toHaveLength(0);
  });

  it('keeps A-B-A reentry as the latest draft and starts a fresh Back afterward', async () => {
    const { router, port, release, request } = await pendingDraftBack();
    act(() => {
      port().go({ name: 'new-track', areaId: 'project-area' });
      port().go({ name: 'new-track', areaId: 'daily-area' });
      port().go({ name: 'new-track', areaId: 'project-area' });
      release();
    });
    await waitFor(() => { expect(router.state.location.pathname).toBe('/area/project-area/new'); expect(router.state.status).toBe('idle'); });
    act(() => { port().back!(); });
    expect(request).toHaveBeenCalledTimes(2);
    act(() => { release(); });
    await waitFor(() => { expect(router.state.location.href).toBe('/track/project?from=pages#source'); });
  });

  it('rejects a BACK delivered at its origin and admits the next navigation', async () => {
    const { router, port } = await pendingDraftBack();
    const latest = vi.fn();
    act(() => {
      port().owner.go(latest);
      // Native blocker rollback publicly reports this shape: BACK, but the
      // delivered location remains the draft. It is not a successful ack.
      router.history.notify({ type: 'BACK' });
    });
    await waitFor(() => { expect(router.state.status).toBe('idle'); });
    act(() => { port().go({ name: 'settings' }); });
    await waitFor(() => { expect(router.state.location.pathname).toBe('/settings'); });
    expect(latest).not.toHaveBeenCalled();
  });

  it('cancels the old continuation when direct navigate takes ownership', async () => {
    const { router, port, release } = await pendingDraftBack();
    const latest = vi.fn();
    act(() => { port().owner.go(latest); });
    await act(async () => { await router.navigate({ to: '/settings' }); });
    act(() => { release(); });
    await waitFor(() => { expect(router.state.status).toBe('idle'); expect(router.state.location.pathname).toBe('/area/project-area/new'); });
    expect(latest).not.toHaveBeenCalled();
  });

  it('cancels on a second traversal observed before route readiness', async () => {
    const { router, port, release } = await pendingDraftBack();
    const latest = vi.fn();
    act(() => { port().owner.go(latest); release(); router.history.forward(); });
    await waitFor(() => { expect(router.state.status).toBe('idle'); expect(router.state.location.pathname).toBe('/area/project-area/new'); });
    expect(latest).not.toHaveBeenCalled();
  });

  it('cleans up an unmounted owner and does not resurrect its continuation on remount', async () => {
    const { router, port, release } = await pendingDraftBack();
    const latest = vi.fn();
    act(() => { port().owner.go(latest); });
    cleanup();
    release();
    renderDailyFixture({ initial: '/area/project-area/new' });
    await waitFor(() => { expect(router.history.location.pathname).toBe('/next/track/project'); });
    expect(latest).not.toHaveBeenCalled();
  });
});
