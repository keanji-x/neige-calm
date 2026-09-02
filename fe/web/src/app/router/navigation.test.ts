import { describe, expect, it } from 'vitest';

import {
  cardIdFromLocation, cardIdFromSearchString, fromFromLocation, fromFromSearchString,
  hasPanelPushedMarker, hasSpecOpenMarker, panelFromLocation, panelFromSearchString, pathFor,
  renderedMobilePanel, sameWaveSearch, validateWaveSearch,
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

describe('pathFor', () => {
  it('does not embed the card query in the path', () => {
    expect(pathFor({ name: 'wave', waveId: 'w1', cardId: 'c1' })).toBe('/wave/w1');
  });
});

describe('panel and from parsers', () => {
  it('reads a single legal value', () => {
    expect(panelFromSearchString('?panel=cards')).toBe('cards');
    expect(panelFromSearchString('panel=outline&from=cove')).toBe('outline');
    expect(fromFromSearchString('?from=pages')).toBe('pages');
    expect(fromFromSearchString('?from=cove')).toBe('cove');
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
    expect(fromFromSearchString('?from=pages&from=cove')).toBeNull();
  });

  it('reads the raw string in preference to an already-folded object', () => {
    expect(panelFromLocation({ searchStr: '?panel=cards&panel=tasks', search: { panel: 'cards' } })).toBeNull();
    expect(fromFromLocation({ searchStr: '?from=pages&from=cove', search: { from: 'pages' } })).toBeNull();
    expect(panelFromLocation({ searchStr: '?panel=tasks' })).toBe('tasks');
    expect(fromFromLocation({ search: { from: 'cove' } })).toBe('cove');
  });
});

describe('validateWaveSearch', () => {
  it('keeps the three whitelisted fields and drops everything else', () => {
    expect(validateWaveSearch({ panel: 'tasks', from: 'cove', other: 'x' }))
      .toEqual({ panel: 'tasks', from: 'cove' });
  });

  it('drops illegal values, empty strings, and repeated keys parsed as arrays', () => {
    expect(validateWaveSearch({ panel: 'report', from: 'today', card: '' })).toEqual({});
    expect(validateWaveSearch({ panel: ['cards', 'tasks'], from: ['pages'] })).toEqual({});
    expect(validateWaveSearch({})).toEqual({});
  });

  it('[#1191 §0.1] drops the panel when a card is present', () => {
    expect(validateWaveSearch({ card: 'c1', panel: 'cards', from: 'pages' }))
      .toEqual({ card: 'c1', from: 'pages' });
  });
});

describe('sameWaveSearch', () => {
  const onW1 = { pathname: '/wave/w1', searchStr: '?card=c1&from=cove' };

  it('refuses a location that is not on the expected wave', () => {
    expect(sameWaveSearch(onW1, 'w2', { card: undefined })).toBeNull();
    expect(sameWaveSearch({ pathname: '/', searchStr: '' }, 'w1', {})).toBeNull();
    expect(sameWaveSearch({ pathname: '/cove/w1', searchStr: '' }, 'w1', {})).toBeNull();
  });

  it('keeps the fields the patch does not mention', () => {
    expect(sameWaveSearch(onW1, 'w1', {})).toEqual({ card: 'c1', from: 'cove' });
    expect(sameWaveSearch(onW1, 'w1', { card: undefined })).toEqual({ from: 'cove' });
  });

  it('distinguishes an absent key from an explicit undefined', () => {
    const location = { pathname: '/wave/w1', searchStr: '?panel=tasks&from=pages' };
    expect(sameWaveSearch(location, 'w1', { from: undefined })).toEqual({ panel: 'tasks' });
    expect(sameWaveSearch(location, 'w1', {})).toEqual({ panel: 'tasks', from: 'pages' });
  });

  it('rebuilds from the whitelist instead of spreading the previous search', () => {
    const polluted = {
      pathname: '/wave/w1',
      searchStr: '?panel=tasks&debug=1&card=a&card=b&from=cove',
    };
    // `debug` never crosses; the duplicated `card` is rejected rather than
    // carried through as an array.
    expect(sameWaveSearch(polluted, 'w1', {})).toEqual({ panel: 'tasks', from: 'cove' });
  });

  it('[#1191 §0.1] clears the panel whenever a card is set', () => {
    const location = { pathname: '/wave/w1', searchStr: '?panel=cards&from=pages' };
    expect(sameWaveSearch(location, 'w1', { card: 'c9' })).toEqual({ card: 'c9', from: 'pages' });
  });

  /*
   * §1.4's first row, and the one combination the two neighbours above miss:
   * they cover a card that never parsed and a card being *set*, never a card
   * being *cleared* off a URL that carries both.
   *
   * The card/panel exclusion belongs to the search this builds, not to the one
   * it reads. Applied while reading, `?card=bad&panel=tasks` normalises to the
   * card alone, and the bounce's `{ card: undefined }` then has no panel left
   * to keep — the reader loses the panel they were in because a *different*
   * parameter was unopenable.
   */
  it('[#1191 §1.4] keeps the panel when the illegal-card bounce clears the card', () => {
    const location = { pathname: '/wave/w1', searchStr: '?card=bad&panel=tasks&from=cove' };
    expect(sameWaveSearch(location, 'w1', { card: undefined })).toEqual({ panel: 'tasks', from: 'cove' });
    // Reading alone still never emits the pair: the patch is what releases it.
    expect(sameWaveSearch(location, 'w1', {})).toEqual({ card: 'bad', from: 'cove' });
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

/*
 * The same six cells as its neighbour, for the same reason: history state is
 * whatever a previous version of this app, or a hand-edited session entry, left
 * behind, so the marker is read as an exact `true` and never as truthiness. A
 * `'true'` string arming the spec drawer would open a conversation and take the
 * caret on an ordinary visit (#1211 S2).
 */
describe('hasSpecOpenMarker', () => {
  it('accepts only the exact marker', () => {
    expect(hasSpecOpenMarker({ ncOpenSpec: true })).toBe(true);
    expect(hasSpecOpenMarker({ ncOpenSpec: false })).toBe(false);
    expect(hasSpecOpenMarker({ ncOpenSpec: 'true' })).toBe(false);
    expect(hasSpecOpenMarker({})).toBe(false);
    expect(hasSpecOpenMarker(null)).toBe(false);
    expect(hasSpecOpenMarker(undefined)).toBe(false);
  });
});

describe('renderedMobilePanel', () => {
  /*
   * The half of the desktop fix that no integration test can see: the route
   * also clears `?panel=` from the URL in an effect, and effects flush inside
   * `act`, so by the time jsdom can query the DOM the URL is already honest.
   * A real browser paints that frame — with the desktop panel `inert` and
   * `aria-hidden` behind a `display: none` mobile list — so the guard is
   * asserted where it is decidable.
   */
  it('[#1191] refuses to open a panel above the compact breakpoint', () => {
    expect(renderedMobilePanel('cards', { compact: false, cardOpen: false })).toBeNull();
    expect(renderedMobilePanel('cards', { compact: true, cardOpen: false })).toBe('cards');
  });

  it('[#1191 §0.1] yields the surface to a live card overlay', () => {
    expect(renderedMobilePanel('tasks', { compact: true, cardOpen: true })).toBeNull();
    expect(renderedMobilePanel(null, { compact: true, cardOpen: false })).toBeNull();
  });
});
