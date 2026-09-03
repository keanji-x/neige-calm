import { describe, expect, it } from 'vitest';

import { DOCK_ITEMS, dockSelection, type DockKey } from './dock.ts';

/*
 * #1191 §3.3. The dock's selection rule used to be four `aria-current`
 * expressions that only added up to "exactly one is lit" if you read all four
 * together; as a function it can be asked directly, including for the case that
 * had no name before — a route the dock has no tab for.
 */
describe('dockSelection', () => {
  const cases: readonly (readonly [Parameters<typeof dockSelection>[0], string, DockKey])[] = [
    [null, '/', 'today'],
    [null, '/settings', 'me'],
    [null, '/settings/appearance', 'me'],
    [null, '/track/w1', 'pages'],
    [null, '/area/c1', 'pages'],
    // A path the dock has no tab for still lights exactly one: Pages is the
    // index the reader is inside, not a fifth "nothing" state.
    [null, '/anything-else', 'pages'],
    // An open sheet is what the reader is looking at, so it wins over the route
    // underneath it — including over Today and Settings.
    ['pages', '/', 'pages'],
    ['areas', '/', 'areas'],
    ['areas', '/settings', 'areas'],
    ['pages', '/track/w1', 'pages'],
  ];
  for (const [section, path, expected] of cases) {
    it(`selects ${expected} for ${String(section)} at ${path}`, () => {
      expect(dockSelection(section, path)).toBe(expected);
    });
  }

  it('never lights more than one item, and always lights one', () => {
    for (const [section, path] of cases) {
      const selected = dockSelection(section, path);
      expect(DOCK_ITEMS.filter((item) => item.key === selected)).toHaveLength(1);
    }
  });

  // `/settings-of-someone-else` is a different route, not a settings sub-page.
  it('does not treat a settings-prefixed path as settings', () => {
    expect(dockSelection(null, '/settingsish')).toBe('pages');
  });
});

describe('DOCK_ITEMS', () => {
  it('gives aria-controls only to the two items that open the sheet', () => {
    expect(DOCK_ITEMS.filter((item) => item.opensSection !== undefined).map((item) => item.key))
      .toEqual(['pages', 'areas']);
    // Today and Me navigate; claiming to control the sheet region would be a
    // lie to a screen reader, not a missing attribute (§3.3).
    expect(DOCK_ITEMS.filter((item) => item.opensSection === undefined).map((item) => item.key))
      .toEqual(['today', 'me']);
  });

  it('is frozen all the way down', () => {
    expect(Object.isFrozen(DOCK_ITEMS)).toBe(true);
    for (const item of DOCK_ITEMS) expect(Object.isFrozen(item)).toBe(true);
  });

  it('has one item per key', () => {
    expect(new Set(DOCK_ITEMS.map((item) => item.key)).size).toBe(DOCK_ITEMS.length);
  });
});
