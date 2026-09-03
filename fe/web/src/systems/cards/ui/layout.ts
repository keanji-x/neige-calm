// Copied from web/src/TrackGrid.tsx packing: 12-col row-major auto-place,
// then reuse stored positions when the user has dragged or resized.

import type { CardSize } from '../registry.js';

export const GRID_COLS = 12;
export const GRID_ROW_HEIGHT = 40;
export const GRID_MARGIN = 14;

export type GridPlacement = Readonly<{
  id: string;
  x: number;
  y: number;
  w: number;
  h: number;
  minW: number;
  minH: number;
}>;

export type StoredPosition = Readonly<{
  x: number;
  y: number;
  w: number;
  h: number;
}>;

export type StoredPositions = Readonly<Record<string, StoredPosition>>;

export function packCards(
  cards: readonly Readonly<{ id: string; size: CardSize }>[],
): readonly GridPlacement[] {
  return reconcileLayout(cards, {});
}

export function reconcileLayout(
  cards: readonly Readonly<{ id: string; size: CardSize }>[],
  stored: StoredPositions,
): readonly GridPlacement[] {
  let nextY = 0;
  for (const card of cards) {
    const entry = stored[card.id];
    if (entry !== undefined) nextY = Math.max(nextY, entry.y + entry.h);
  }
  let cursorX = 0;
  let rowH = 0;
  const result: GridPlacement[] = [];
  for (const card of cards) {
    const size = card.size;
    const entry = stored[card.id];
    if (entry !== undefined) {
      result.push(Object.freeze({
        id: card.id,
        x: entry.x,
        y: entry.y,
        w: entry.w,
        h: entry.h,
        minW: size.minW,
        minH: size.minH,
      }));
      continue;
    }
    if (cursorX + size.w > GRID_COLS) {
      cursorX = 0;
      nextY += rowH;
      rowH = 0;
    }
    result.push(Object.freeze({
      id: card.id,
      x: cursorX,
      y: nextY,
      w: size.w,
      h: size.h,
      minW: size.minW,
      minH: size.minH,
    }));
    cursorX += size.w;
    rowH = Math.max(rowH, size.h);
  }
  return Object.freeze(result);
}

export function layoutToPositions(
  layout: readonly Readonly<{ i: string; x: number; y: number; w: number; h: number }>[],
): StoredPositions {
  const out: Record<string, StoredPosition> = {};
  for (const item of layout) {
    out[item.i] = Object.freeze({ x: item.x, y: item.y, w: item.w, h: item.h });
  }
  return Object.freeze(out);
}
