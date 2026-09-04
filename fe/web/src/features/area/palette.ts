/**
 * INV-DUP-006 — the single area colour palette.
 *
 * AppShell's Area-editor create flow is the palette's sole consumer. It picks
 * one of these values at random and sends it to the kernel as the Area colour.
 */
export const AREA_PALETTE = Object.freeze([
  '#5B8DEF', '#8B7FE8', '#E88B7F', '#7FC8A9', '#E8C97F', '#C87FA9',
] as const);
