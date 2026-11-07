import { describe, expect, it } from 'vitest';
import { inPreviewBridge, placePreview, type Bounds } from './placement.ts';

const box = (left: number, top: number, width: number, height: number): Bounds => ({ left, top, right: left + width, bottom: top + height });
const viewport = { width: 1440, height: 900 };

describe('preview placement', () => {
  it('keeps a chat preview near its link even when that overlaps reading areas', () => {
    const anchor = box(1750, 380, 130, 24);
    const chat = box(1300, 60, 600, 800);
    const report = box(450, 60, 700, 800);
    const result = placePreview({ anchor, reading: chat, readingAreas: [report, chat], obstacles: [],
      viewport: { width: 1920, height: 900 } })!;
    expect(result.x + result.width).toBeGreaterThanOrEqual(anchor.left - 12);
    expect(result.x).toBeLessThanOrEqual(anchor.right + 12);
  });

  it('prefers a nearby preview over an uncovered position one card width away', () => {
    const anchor = box(1330, 300, 80, 24);
    const chat = box(900, 60, 520, 800);
    const result = placePreview({ anchor, reading: chat, readingAreas: [box(100, 60, 330, 800), chat],
      obstacles: [], viewport })!;
    const right = result.x + result.width;
    const bottom = result.y + result.maxHeight;
    const gap = Math.max(result.x - anchor.right, anchor.left - right, result.y - anchor.bottom, anchor.top - bottom);
    expect(gap).toBe(12);
  });

  it('keeps a full-height card when nearby vertical space is cramped', () => {
    const anchor = box(1200, 185, 80, 24);
    const chat = box(900, 60, 520, 800);
    const result = placePreview({ anchor, reading: chat, readingAreas: [box(100, 60, 330, 800), chat],
      obstacles: [], viewport })!;
    expect(result.side).toBe('left');
    expect(result.maxHeight).toBe(544);
    expect(anchor.left - (result.x + result.width)).toBe(12);
  });

  it('keeps a report preview on the open left side next to its link', () => {
    const anchor = box(480, 280, 300, 24);
    const report = box(470, 60, 700, 800);
    const result = placePreview({ anchor, reading: report, readingAreas: [report, box(1300, 60, 600, 800)],
      obstacles: [], viewport: { width: 1920, height: 900 } })!;
    expect(result.side).toBe('left');
    expect(anchor.left - (result.x + result.width)).toBeLessThanOrEqual(24);
  });

  it('keeps the preview close when blank space lies farther away', () => {
    const result = placePreview({ anchor: box(260, 300, 80, 24), reading: box(200, 100, 504, 700), obstacles: [], viewport });
    expect(result).toMatchObject({ side: 'right', x: 352, width: 448 });
  });
  it('uses the left side when the right is occupied', () => {
    const result = placePreview({ anchor: box(800, 300, 80, 24), reading: box(700, 100, 504, 700), obstacles: [box(1216, 100, 200, 700)], viewport });
    expect(result).toMatchObject({ side: 'left', x: 340, width: 448 });
  });
  it('uses nearby space within a narrower viewport', () => {
    const result = placePreview({ anchor: box(260, 300, 80, 24), reading: box(200, 100, 504, 700), obstacles: [], viewport: { width: 1040, height: 900 } });
    expect(result).toMatchObject({ side: 'right', x: 352, width: 448 });
  });
  it('caps a narrow viewport fallback above the source instead of covering it', () => {
    const anchor = box(60, 500, 200, 24);
    const result = placePreview({ anchor, reading: box(24, 100, 342, 1000), obstacles: [], viewport: { width: 390, height: 650 } })!;
    expect(result.side).toBe('above');
    expect(result.y + result.maxHeight).toBeLessThanOrEqual(anchor.top - 12);
    expect(result.x + result.width).toBeLessThanOrEqual(378);
  });
  it('keeps a child outside its parent when close to the original reading area', () => {
    const parent = box(716, 250, 448, 544);
    const result = placePreview({ anchor: box(740, 500, 80, 24), reading: parent,
      readingAreas: [box(200, 100, 504, 1000)], obstacles: [parent], viewport })!;
    expect(result.x + result.width).toBeLessThanOrEqual(parent.left);
  });
  it('reserves the larger legal space beside a narrow source', () => {
    const input = { anchor: box(60, 300, 80, 24), reading: box(24, 100, 342, 1000), obstacles: [], viewport: { width: 390, height: 650 } };
    expect(placePreview(input)).toEqual({ x: 12, y: 336, width: 366, maxHeight: 302, side: 'below' });
  });
  it('does not use off-screen reading areas as off-screen placement destinations', () => {
    const result = placePreview({ anchor: box(260, 300, 80, 24), reading: box(200, 100, 504, 700),
      readingAreas: [box(200, 100, 504, 700), box(200, 1500, 504, 200)], obstacles: [],
      viewport: { width: 1040, height: 900 } })!;
    expect(result.x).toBeGreaterThanOrEqual(12);
    expect(result.x + result.width).toBeLessThanOrEqual(1028);
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
