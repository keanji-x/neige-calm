/**
 * INV-DUP-006 — the single cove colour palette.
 *
 * The values are shared, the *picking strategy* is deliberately not: the wave
 * create form picks deterministically by cove count so a burst of creates
 * spreads across the palette, while the sidebar's inline new-cove picks at
 * random. Merging the two strategies was explicitly ruled out — only the
 * values converge here.
 */
export const COVE_PALETTE = Object.freeze([
  '#5B8DEF', '#8B7FE8', '#E88B7F', '#7FC8A9', '#E8C97F', '#C87FA9',
] as const);

/** Deterministic pick, for forms that know how many coves already exist. */
export function coveColorForIndex(index: number): string {
  return COVE_PALETTE[((index % COVE_PALETTE.length) + COVE_PALETTE.length) % COVE_PALETTE.length];
}
