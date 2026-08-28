import { describe, expect, it } from 'vitest';

import { GRID_COLS, layoutToPositions, packCards, reconcileLayout } from './layout.ts';

const size = Object.freeze({ w: 4, h: 6, minW: 3, minH: 3 });

describe('packCards (WaveGrid reconcile)', () => {
  it('packs three 4-wide terminals on one 12-col row', () => {
    const layout = packCards([
      { id: 'a', size }, { id: 'b', size }, { id: 'c', size },
    ]);
    expect(layout).toEqual([
      { id: 'a', x: 0, y: 0, w: 4, h: 6, minW: 3, minH: 3 },
      { id: 'b', x: 4, y: 0, w: 4, h: 6, minW: 3, minH: 3 },
      { id: 'c', x: 8, y: 0, w: 4, h: 6, minW: 3, minH: 3 },
    ]);
  });

  it('wraps the fourth 4-wide card onto the next row', () => {
    const layout = packCards([
      { id: 'a', size }, { id: 'b', size }, { id: 'c', size }, { id: 'd', size },
    ]);
    expect(layout[3]).toEqual({ id: 'd', x: 0, y: 6, w: 4, h: 6, minW: 3, minH: 3 });
    const fourth = layout[3];
    expect(fourth.x + fourth.w).toBeLessThanOrEqual(GRID_COLS);
  });
});

describe('reconcileLayout', () => {
  it('keeps a stored position and packs a newcomer at the bottom', () => {
    const layout = reconcileLayout(
      [{ id: 'a', size }, { id: 'b', size }],
      { a: { x: 6, y: 2, w: 5, h: 8 } },
    );
    expect(layout[0]).toEqual({ id: 'a', x: 6, y: 2, w: 5, h: 8, minW: 3, minH: 3 });
    expect(layout[1]).toEqual({ id: 'b', x: 0, y: 10, w: 4, h: 6, minW: 3, minH: 3 });
  });
});

describe('layoutToPositions', () => {
  it('drops min size and keys by RGL item id', () => {
    expect(layoutToPositions([
      { i: 'a', x: 1, y: 2, w: 3, h: 4 },
    ])).toEqual({ a: { x: 1, y: 2, w: 3, h: 4 } });
  });
});
