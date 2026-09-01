import { describe, expect, it } from 'vitest';

import {
  cardIdFromLocation, cardIdFromSearchString, fromFromLocation, fromFromSearchString,
  hasPanelPushedMarker, panelFromLocation, panelFromSearchString, pathFor,
  sameWaveSearch, validateWaveSearch,
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
