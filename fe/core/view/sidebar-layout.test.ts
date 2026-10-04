import { describe, expect, it } from 'vitest';
import { moveSidebarGroup, sidebarOrder } from './sidebar-layout.js';

describe('sidebar presentation order', () => {
  it('never loses new groups when saved order contains stale or repeated IDs', () => {
    expect(sidebarOrder(['a', 'b', 'new'], ['b', 'deleted', 'b', 'a'])).toEqual(['b', 'a', 'new']);
  });
  it('uses registered order for malformed storage', () => {
    for (const saved of [null, false, {}, 'a', ['a', 2]]) expect(sidebarOrder(['a', 'b'], saved)).toEqual(['a', 'b']);
  });
  it('keeps hidden slots and swaps only the requested visible siblings', () => {
    const source = ['a', 'hidden', 'b', 'c'];
    expect(moveSidebarGroup(source, ['a', 'b', 'c'], 'a', 'down')).toEqual(['b', 'hidden', 'a', 'c']);
    expect(moveSidebarGroup(source, ['a', 'b', 'c'], 'c', 'up')).toEqual(['a', 'hidden', 'c', 'b']);
    expect(source).toEqual(['a', 'hidden', 'b', 'c']);
  });
  it('does not move past either boundary or move an unavailable ID', () => {
    for (const [id, direction] of [['a', 'up'], ['b', 'down'], ['deleted', 'down']] as const) {
      expect(moveSidebarGroup(['a', 'b'], ['a', 'b'], id, direction)).toEqual(['a', 'b']);
    }
  });
});
