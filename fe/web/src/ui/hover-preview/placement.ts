export type Bounds = Readonly<{ left: number; top: number; right: number; bottom: number }>;
export type Side = 'right' | 'left' | 'below' | 'above';
export type Placement = Readonly<{ x: number; y: number; width: number; maxHeight: number; side: Side }>;
export type Point = Readonly<{ x: number; y: number }>;

export const PREVIEW_GAP = 12;
export const PREVIEW_WIDTH = 448;
export const PREVIEW_MAX_HEIGHT = 544;
const MIN_WIDTH = 280;
export const PREVIEW_MIN_HEIGHT = 160;

function overlap(a: Bounds, b: Bounds): number {
  return Math.max(0, Math.min(a.right, b.right) - Math.max(a.left, b.left))
    * Math.max(0, Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top));
}
function clamp(value: number, low: number, high: number): number {
  return Math.max(low, Math.min(value, high));
}

/** Protect the source first, then existing previews and the host's reading area.
 * Side space may narrow the card. Vertical fallbacks scroll within the available
 * height; no legal placement means no popup, rather than hiding its own trigger. */
export function placePreview({ anchor, reading, readingAreas, obstacles, viewport, naturalHeight }: Readonly<{
  anchor: Bounds;
  reading: Bounds | null;
  readingAreas?: readonly Bounds[];
  obstacles: readonly Bounds[];
  viewport: Readonly<{ width: number; height: number }>;
  naturalHeight: number;
}>): Placement | null {
  const width = Math.max(0, Math.min(PREVIEW_WIDTH, viewport.width - 2 * PREVIEW_GAP));
  const height = Math.max(0, Math.min(PREVIEW_MAX_HEIGHT, viewport.height - 2 * PREVIEW_GAP));
  if (width === 0 || height === 0) return null;
  const desiredHeight = Math.min(naturalHeight, height);
  type Candidate = Readonly<{ placement: Placement; occupied: number; reading: number; sideRank: number; lost: number; order: number }>;
  const candidates: Candidate[] = [];
  const areas = readingAreas ?? (reading === null ? [] : [reading]);
  const boundaries = [reading ?? anchor, anchor, ...obstacles, ...areas];
  const add = (placement: Placement, minimumHeight: number) => {
    if (placement.width < Math.min(MIN_WIDTH, width) || placement.maxHeight < minimumHeight) return;
    const actualHeight = Math.min(desiredHeight, placement.maxHeight);
    const bounds = { left: placement.x, right: placement.x + placement.width, top: placement.y, bottom: placement.y + actualHeight };
    // Score the space a loaded card can occupy, so a short loading state does
    // not select a different side from the completed resource.
    const reservedTop = placement.side === 'above' ? bounds.bottom - placement.maxHeight
      : placement.side === 'below' ? placement.y
        : clamp(anchor.top, PREVIEW_GAP, viewport.height - PREVIEW_GAP - placement.maxHeight);
    const reserved = { ...bounds, top: reservedTop, bottom: reservedTop + placement.maxHeight };
    if (bounds.left < PREVIEW_GAP || bounds.top < PREVIEW_GAP || bounds.right > viewport.width - PREVIEW_GAP
      || bounds.bottom > viewport.height - PREVIEW_GAP || reserved.top < PREVIEW_GAP
      || reserved.bottom > viewport.height - PREVIEW_GAP) return;
    if (overlap(bounds, anchor) > 0 || overlap(reserved, anchor) > 0) return;
    const side: Side = bounds.left >= anchor.right ? 'right' : bounds.right <= anchor.left ? 'left'
      : bounds.top >= anchor.bottom ? 'below' : 'above';
    candidates.push({ placement: { ...placement, side }, occupied: obstacles.reduce((area, obstacle) => area + overlap(reserved, obstacle), 0),
      reading: areas.reduce((area, occupied) => area + overlap(reserved, occupied), 0),
      sideRank: side === 'right' || side === 'left' ? 0 : 1,
      lost: width * height - placement.width * placement.maxHeight, order: candidates.length });
  };
  for (const minimumHeight of [Math.min(PREVIEW_MIN_HEIGHT, height), 1]) {
    for (const boundary of boundaries) {
      const rightWidth = Math.min(width, viewport.width - PREVIEW_GAP - boundary.right - PREVIEW_GAP);
      const leftWidth = Math.min(width, boundary.left - 2 * PREVIEW_GAP);
      const y = clamp(anchor.top, PREVIEW_GAP, viewport.height - PREVIEW_GAP - desiredHeight);
      add({ x: boundary.right + PREVIEW_GAP, y, width: rightWidth, maxHeight: height, side: 'right' }, minimumHeight);
      add({ x: boundary.left - PREVIEW_GAP - leftWidth, y, width: leftWidth, maxHeight: height, side: 'left' }, minimumHeight);
      const below = Math.min(height, viewport.height - PREVIEW_GAP - boundary.bottom - PREVIEW_GAP);
      const above = Math.min(height, boundary.top - 2 * PREVIEW_GAP);
      const x = clamp(anchor.left, PREVIEW_GAP, viewport.width - PREVIEW_GAP - width);
      add({ x, y: boundary.bottom + PREVIEW_GAP, width, maxHeight: below, side: 'below' }, minimumHeight);
      add({ x, y: boundary.top - PREVIEW_GAP - Math.min(desiredHeight, above), width, maxHeight: above, side: 'above' }, minimumHeight);
    }
    if (candidates.length > 0) break;
  }
  candidates.sort((a, b) => a.occupied - b.occupied || a.reading - b.reading || a.sideRank - b.sideRank || a.lost - b.lost || a.order - b.order);
  return candidates[0]?.placement ?? null;
}

/** A geometric travel corridor never intercepts clicks on the underlying page. */
export function inPreviewBridge(point: Point, anchor: Bounds, card: Bounds, side: Side): boolean {
  const pad = 6;
  let polygon: readonly Point[];
  switch (side) {
    case 'right': polygon = [{ x: anchor.right, y: anchor.top - pad }, { x: card.left, y: card.top - pad }, { x: card.left, y: card.bottom + pad }, { x: anchor.right, y: anchor.bottom + pad }]; break;
    case 'left': polygon = [{ x: card.right, y: card.top - pad }, { x: anchor.left, y: anchor.top - pad }, { x: anchor.left, y: anchor.bottom + pad }, { x: card.right, y: card.bottom + pad }]; break;
    case 'below': polygon = [{ x: anchor.left - pad, y: anchor.bottom }, { x: anchor.right + pad, y: anchor.bottom }, { x: card.right + pad, y: card.top }, { x: card.left - pad, y: card.top }]; break;
    case 'above': polygon = [{ x: card.left - pad, y: card.bottom }, { x: card.right + pad, y: card.bottom }, { x: anchor.right + pad, y: anchor.top }, { x: anchor.left - pad, y: anchor.top }]; break;
  }
  let positive = false;
  let negative = false;
  for (let index = 0; index < polygon.length; index += 1) {
    const start = polygon[index];
    const end = polygon[(index + 1) % polygon.length];
    const cross = (end.x - start.x) * (point.y - start.y) - (end.y - start.y) * (point.x - start.x);
    positive ||= cross > 0;
    negative ||= cross < 0;
  }
  return !(positive && negative);
}
