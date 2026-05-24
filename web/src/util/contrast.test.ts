// Tests for pickFgForBg — the colour the Calendar's wave bars use for
// title + cove subscript. The contract is "always passes WCAG AA against
// the bar's painted background"; the cases below pin the common-case
// inputs that a user-picked cove colour might be, plus the malformed
// fallback path.

import { describe, expect, it } from 'vitest';
import { pickFgForBg } from './contrast';

describe('pickFgForBg', () => {
  it('returns black against pure white', () => {
    expect(pickFgForBg('#ffffff')).toBe('#000');
  });

  it('returns white against pure black', () => {
    expect(pickFgForBg('#000000')).toBe('#fff');
  });

  it('returns white against a saturated blue (#0066cc)', () => {
    // Y ≈ 0.121 → white wins (4.5+ vs ~2.3 for black).
    expect(pickFgForBg('#0066cc')).toBe('#fff');
  });

  it('returns black against bright yellow (#ffff00)', () => {
    // Y ≈ 0.928 → black wins overwhelmingly.
    expect(pickFgForBg('#ffff00')).toBe('#000');
  });

  it('returns black against mid grey (#808080)', () => {
    // Y ≈ 0.216 → at the WCAG analytic crossover (~0.179) we sit
    // *above* it, so black gives the larger contrast. This pins the
    // tie-break direction so a refactor that swaps the inequality
    // (>= → >) doesn't silently flip mid-grey bars.
    expect(pickFgForBg('#808080')).toBe('#000');
  });

  it('handles 3-digit hex (e.g. cove fixture "#5a9")', () => {
    // The Calendar test fixture uses `#5a9` for the Atlas cove.
    // 0x55, 0xaa, 0x99 → Y ≈ 0.30 → black is the higher-contrast pick.
    expect(pickFgForBg('#5a9')).toBe('#000');
  });

  it('handles rgb() function form', () => {
    expect(pickFgForBg('rgb(0, 0, 0)')).toBe('#fff');
    expect(pickFgForBg('rgb(255, 255, 255)')).toBe('#000');
  });

  it('handles rgba() function form and ignores alpha for the luminance calc', () => {
    // Alpha is intentionally ignored — the bar paints `cove.color` as
    // a fully opaque fill; the text just needs to read against that
    // perceived colour. Same RGB → same fg regardless of alpha.
    expect(pickFgForBg('rgba(0, 102, 204, 0.5)')).toBe('#fff');
  });

  it('handles 8-digit hex (#rrggbbaa) by ignoring the alpha channel', () => {
    expect(pickFgForBg('#0066ccff')).toBe('#fff');
  });

  it('falls back to black for an unparseable string (defensive)', () => {
    // CSS variables like `var(--text-3)` reach the helper via
    // Calendar.tsx's `cove?.color ?? 'var(--text-3)'` fallback when a
    // cove is missing; we can't compute luminance from that, but the
    // bar is using a token surface in that case so the default-light
    // `#000` reads correctly anyway.
    expect(pickFgForBg('var(--text-3)')).toBe('#000');
    expect(pickFgForBg('not-a-colour')).toBe('#000');
    expect(pickFgForBg('')).toBe('#000');
  });
});
