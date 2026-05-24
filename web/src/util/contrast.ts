// Pick a foreground colour (black or white) that reads against an
// arbitrary background colour string. Used by Calendar's WaveBar so
// the cove-coloured bar always carries WCAG-AA-passing text regardless
// of whether the user picked a pastel green or a deep navy for the cove.
//
// Why this exists: the bar background is whatever colour the user
// chose for the cove (`cove.color` — any CSS-parseable RGB / hex
// string). Pre-fix the bar text fell back to `var(--text)`-derived
// values, which were near-black in light mode and near-white in dark
// mode. When the cove colour happens to land on the same end of the
// scale as the page text (e.g. a light-green cove in light mode, or a
// deep-navy cove in dark mode) contrast collapsed below 4.5:1 and
// axe-core flagged it (`color-contrast`, serious). Computing the
// foreground per-bar from the actual bar background sidesteps the
// problem regardless of theme.
//
// Algorithm: parse the colour into 0..255 RGB, compute relative
// luminance per WCAG 2.1 (sRGB → linear → Y), and pick whichever of
// black / white gives the larger contrast ratio. White text wins on
// dark backgrounds (Y < ~0.179), black wins on light. The 0.179
// crossover is the analytic WCAG point where contrast(white) ==
// contrast(black); we don't hardcode the constant, we just compare
// both ratios so the function stays robust if a future caller wants
// to plug in non-black/white candidates.
//
// Inputs we accept:
//   - 3-digit hex:  #abc  → #aabbcc
//   - 6-digit hex:  #aabbcc
//   - 8-digit hex:  #aabbccdd (alpha dropped — we colour the foreground,
//                              not the background, so the alpha doesn't
//                              affect the perceived bar paint at this
//                              point. Calendar's bar never uses alpha
//                              on `cove.color`.)
//   - rgb(r, g, b) / rgba(r, g, b, a) — integer or float components,
//     percentages NOT supported (no caller produces them).
//
// Anything we can't parse falls through to the "assume dark text on a
// light bg" default — same colour the pre-fix CSS would have picked,
// so we degrade gracefully rather than throwing on a malformed string.

/**
 * Pick a contrasting foreground colour (`'#000'` or `'#fff'`) for the
 * given background colour string. Falls back to `'#000'` for unparseable
 * input so the caller never has to handle null.
 */
export function pickFgForBg(bg: string): '#000' | '#fff' {
  const rgb = parseRgb(bg);
  if (!rgb) return '#000';
  const y = relativeLuminance(rgb);
  // Contrast ratio per WCAG 2.1: (L1 + 0.05) / (L2 + 0.05) with L1 >= L2.
  // White has L = 1; black has L = 0. We pick whichever gives the larger
  // ratio against the bar's luminance Y.
  const contrastWhite = (1 + 0.05) / (y + 0.05);
  const contrastBlack = (y + 0.05) / (0 + 0.05);
  return contrastWhite >= contrastBlack ? '#fff' : '#000';
}

interface Rgb {
  r: number;
  g: number;
  b: number;
}

/** Parse `#rgb` / `#rrggbb` / `#rrggbbaa` / `rgb(...)` / `rgba(...)`. */
function parseRgb(input: string): Rgb | null {
  const s = input.trim();
  if (s.startsWith('#')) return parseHex(s);
  if (s.startsWith('rgb')) return parseRgbFunc(s);
  return null;
}

function parseHex(s: string): Rgb | null {
  const hex = s.slice(1);
  if (hex.length === 3) {
    // #abc → #aabbcc
    const r = parseInt(hex[0] + hex[0], 16);
    const g = parseInt(hex[1] + hex[1], 16);
    const b = parseInt(hex[2] + hex[2], 16);
    if ([r, g, b].some(Number.isNaN)) return null;
    return { r, g, b };
  }
  if (hex.length === 6 || hex.length === 8) {
    const r = parseInt(hex.slice(0, 2), 16);
    const g = parseInt(hex.slice(2, 4), 16);
    const b = parseInt(hex.slice(4, 6), 16);
    if ([r, g, b].some(Number.isNaN)) return null;
    return { r, g, b };
  }
  return null;
}

function parseRgbFunc(s: string): Rgb | null {
  // Matches `rgb(r, g, b)` and `rgba(r, g, b, a)` with optional spaces
  // and integer or float components. Percentages are intentionally NOT
  // supported — no caller produces them and adding the branch would
  // double the test surface for zero current value.
  const m = s.match(
    /^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*[\d.]+\s*)?\)$/,
  );
  if (!m) return null;
  const r = Number(m[1]);
  const g = Number(m[2]);
  const b = Number(m[3]);
  if ([r, g, b].some((n) => !Number.isFinite(n))) return null;
  return { r, g, b };
}

/** WCAG 2.1 relative luminance (Y) for an sRGB colour with 0..255 channels. */
function relativeLuminance({ r, g, b }: Rgb): number {
  const lin = (c: number): number => {
    const n = c / 255;
    return n <= 0.03928 ? n / 12.92 : Math.pow((n + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}
