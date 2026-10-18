import { describe, expect, it } from 'vitest';
import { inPreviewBridge, placePreview, type Bounds } from './placement.ts';

const box = (left: number, top: number, width: number, height: number): Bounds => ({ left, top, right: left + width, bottom: top + height });
const viewport = { width: 1440, height: 900 };

describe('preview placement', () => {
  it('uses blank space beside the reading column', () => {
    const result = placePreview({ anchor: box(260, 300, 80, 24), reading: box(200, 100, 504, 700), obstacles: [], viewport });
    expect(result).toMatchObject({ side: 'right', x: 716, width: 448 });
  });
  it('uses the left side when the right is occupied', () => {
    const result = placePreview({ anchor: box(800, 300, 80, 24), reading: box(700, 100, 504, 700), obstacles: [box(1216, 100, 200, 700)], viewport });
    expect(result).toMatchObject({ side: 'left', x: 240, width: 448 });
  });
  it('narrows to usable side space before covering the reading area', () => {
    const result = placePreview({ anchor: box(260, 300, 80, 24), reading: box(200, 100, 504, 700), obstacles: [], viewport: { width: 1040, height: 900 } });
    expect(result).toMatchObject({ side: 'right', x: 716, width: 312 });
  });
  it('caps a narrow viewport fallback above the source instead of covering it', () => {
    const anchor = box(60, 500, 200, 24);
    const result = placePreview({ anchor, reading: box(24, 100, 342, 1000), obstacles: [], viewport: { width: 390, height: 650 } })!;
    expect(result.side).toBe('above');
    expect(result.y + result.maxHeight).toBeLessThanOrEqual(anchor.top - 12);
    expect(result.x + result.width).toBeLessThanOrEqual(378);
  });
  it('keeps a child outside both its parent and the original reading area', () => {
    const parent = box(716, 250, 448, 544);
    const result = placePreview({ anchor: box(740, 500, 80, 24), reading: parent,
      readingAreas: [box(200, 100, 504, 1000)], obstacles: [parent], viewport })!;
    expect(result.x).toBeGreaterThanOrEqual(716);
    expect(result.y + result.maxHeight).toBeLessThanOrEqual(parent.top - 12);
  });
  it('reserves the full legal space above a narrow source', () => {
    const input = { anchor: box(60, 300, 80, 24), reading: box(24, 100, 342, 1000), obstacles: [], viewport: { width: 390, height: 650 } };
    expect(placePreview(input)).toEqual({ x: 12, y: 12, width: 366, maxHeight: 276, side: 'above' });
  });
  it('does not use off-screen reading areas as off-screen placement destinations', () => {
    const result = placePreview({ anchor: box(260, 300, 80, 24), reading: box(200, 100, 504, 700),
      readingAreas: [box(200, 100, 504, 700), box(200, 1500, 504, 200)], obstacles: [],
      viewport: { width: 1040, height: 900 } })!;
    expect(result).toMatchObject({ side: 'right', width: 312 });
    expect(result.y + result.maxHeight).toBeLessThanOrEqual(888);
  });
  it('declines when the trigger occupies the whole viewport', () => {
    expect(placePreview({ anchor: box(0, 0, 390, 650), reading: null, obstacles: [], viewport: { width: 390, height: 650 } })).toBeNull();
  });
});

describe('non-intercepting pointer corridor', () => {
  it.each(['right', 'left', 'below', 'above'] as const)('joins an anchor to a card on the %s', (side) => {
    const anchor = box(500, 400, 80, 24);
    const card = side === 'right' ? box(900, 200, 448, 544) : side === 'left' ? box(20, 200, 448, 544)
      : side === 'below' ? box(400, 650, 448, 200) : box(400, 100, 448, 200);
    const point = side === 'right' ? { x: 700, y: 410 } : side === 'left' ? { x: 484, y: 410 }
      : side === 'below' ? { x: 540, y: 530 } : { x: 540, y: 350 };
    expect(inPreviewBridge(point, anchor, card, side)).toBe(true);
    expect(inPreviewBridge({ x: 5, y: 5 }, anchor, card, side)).toBe(false);
  });
});
