import { describe, expect, it } from 'vitest';

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

/*
 * The same six cells as its neighbour, for the same reason: history state is
 * whatever a previous version of this app, or a hand-edited session entry, left
 * behind, so the marker is read as an exact `true` and never as truthiness. A
 * `'true'` string arming the planner drawer would open a conversation and take the
 * caret on an ordinary visit (#1211 S2).
 */
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
  /*
   * The half of the desktop fix that no integration test can see: the route
   * also clears `?panel=` from the URL in an effect, and effects flush inside
   * `act`, so by the time jsdom can query the DOM the URL is already honest.
   * A real browser paints that frame — with the desktop panel `inert` and
   * `aria-hidden` behind a `display: none` mobile list — so the guard is
   * asserted where it is decidable.
   */
  it('[#1191] refuses to open a panel above the compact breakpoint', () => {
    expect(renderedMobilePanel('cards', { compact: false, overlayOpen: false })).toBeNull();
    expect(renderedMobilePanel('cards', { compact: true, overlayOpen: false })).toBe('cards');
  });

  it('[#1191 §0.1] yields the surface to a live card overlay', () => {
    expect(renderedMobilePanel('tasks', { compact: true, overlayOpen: true })).toBeNull();
    expect(renderedMobilePanel(null, { compact: true, overlayOpen: false })).toBeNull();
  });
});
