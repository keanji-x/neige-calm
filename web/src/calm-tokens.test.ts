/// <reference types="node" />
// Token-graph invariants for `calm.css` — slice 2 of #142.
//
// The intent of slice 1 (#137 + followups) was to establish a two-tier token
// vocabulary in `calm.css`:
//
//   - Positional tokens (`--bg`, `--text-2`, `--accent`, …) declared once in
//     `:root` and re-declared in `[data-theme="dark"]`. They carry concrete
//     `oklch(...)` values and swap on theme.
//   - Semantic aliases (`--text-label`, `--text-meta`, `--surface-paper`, …)
//     declared once in `:root` as `var(...)` references. They resolve through
//     the cascade — a dark override would *defeat* their purpose because the
//     positional token they alias already swaps in dark.
//
// Surface tokens are split:
//   - Concrete surfaces (`--surface-rail`, `--surface-card`, `--surface-chip`,
//     `--surface-toggle-overlay`, `--surface-panel-head`) are positional —
//     they hold oklch literals and need light + dark coverage.
//   - Alias surfaces (`--surface-paper`, `--surface-bg`,
//     `--surface-hover-overlay`) point at other tokens via `var()`. No dark
//     override.
//
// This spec converts those conventions into a CI gate so the next refactor
// can't silently regress them. We parse `calm.css` with a small regex set
// (no postcss / no new dep — see #142 plan note) and pin the expected token
// list at the top of the file. If you add or rename a token in `calm.css`,
// update the matching constant here in the same commit.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { describe, it, expect } from 'vitest';
import { MONO_STACK } from './font-stack';

// ---------------------------------------------------------------------------
// Pinned token vocabulary
// ---------------------------------------------------------------------------
//
// These arrays *are* the contract. Data-driven detection (e.g. "find all
// `--surface-*` and infer which are aliases") was deliberately rejected: the
// whole point of this spec is to make the next "what tokens do we have"
// question a reviewable diff against this file. If you add a token to
// `calm.css`, add it here and the reviewer will see both halves.

// Positional tokens: defined in `:root` with an oklch (or other concrete)
// literal AND redefined in `[data-theme="dark"]`. They swap on theme.
const POSITIONAL_TOKENS = [
  '--bg',
  '--paper',
  '--hairline',
  '--hairline-strong',
  '--text',
  '--text-2',
  '--text-3',
  '--text-4',
  '--accent',
  '--accent-soft',
  '--warn',
  '--warn-soft',
] as const;

// Concrete surface tokens: like positional tokens but in the `--surface-*`
// namespace. Carry oklch literals, defined in both `:root` and dark.
const CONCRETE_SURFACE_TOKENS = [
  '--surface-rail',
  '--surface-card',
  '--surface-chip',
  '--surface-toggle-overlay',
  '--surface-panel-head',
] as const;

// Prose / code surface tokens (slice 3c of #137). Same shape contract as
// CONCRETE_SURFACE_TOKENS — oklch literals in both `:root` and dark — but
// listed separately so the slice-3c migration is auditable as its own
// chunk in the test inventory.
const PROSE_SURFACE_TOKENS = [
  '--surface-terminal',
  '--surface-code',
] as const;

// Diff line palette (slice 3c of #137). Background + text pairs for k-add /
// k-rm rows. Backgrounds carry the 4-arg oklch(... / alpha) form so they
// blend over the card surface; text values are deliberately separate from
// --success / --error (slice 3b) — diff contexts want denser hues.
const DIFF_COLOR_TOKENS = [
  '--diff-add-bg',
  '--diff-rm-bg',
  '--diff-add-text',
  '--diff-rm-text',
] as const;

// Overlay tokens (slice 3a of #137): transparent hover/active overlays.
// 4-tier vocabulary that replaced ~42 inline `oklch(0% 0 0 / 0.0X)` /
// `oklch(100% 0 0 / 0.0X)` literals scattered across hover and selected
// selectors. Like positional + concrete surface tokens they carry oklch
// literals with light + dark parity — the dark literal varies (it
// lightens against the dark background) but both sides are concrete
// oklch, not aliases. Migration was intentionally lossy; the rounding
// policy lives in #137 (slice 3a body).
const OVERLAY_TOKENS = [
  '--overlay-hover-faint',
  '--overlay-hover',
  '--overlay-hover-strong',
  '--overlay-active',
] as const;

// Alias tokens: `var(--other)` references declared once in `:root`. They
// MUST NOT have a dark override — the underlying positional token already
// swaps, and a dark override would just re-pin the alias to a different
// (likely stale) target.
const ALIAS_TOKENS = [
  '--surface-paper',
  '--surface-bg',
  '--surface-hover-overlay',
  '--text-label',
  '--text-meta',
  '--text-decorative',
] as const;

// Type-scale tokens: single-mode (don't theme-vary). Declared once in `:root`
// with a concrete `Npx` literal — they're the consolidated vocabulary that
// every component selector reads through (slice 1 of #150). The migration
// from 20+ raw font-size literals is intentionally lossy; the rounding
// policy lives in the issue body, not here.
//
// Unlike POSITIONAL_TOKENS, these MUST NOT have a dark override: the type
// scale is part of the design system's shape vocabulary (like `--r`), not
// its color vocabulary.
const TYPE_SCALE_TOKENS = [
  '--text-xs',
  '--text-sm',
  '--text-base',
  '--text-md',
  '--text-lg',
  '--text-xl',
  '--text-display-sm',
  '--text-display',
] as const;

// Line-height (leading) scale tokens — slice 3 of #165. Single-mode like the
// type scale: leading is shape, not color, so no dark override. Values are
// unitless ratios (the `Npx` literal of TYPE_SCALE has no analogue here — a
// `1` line-height composes against any inherited font-size). The migration
// from ~10 distinct ratios is intentionally lossy; rounding policy lives in
// the issue body.
const LEADING_TOKENS = [
  '--leading-none',
  '--leading-tight',
  '--leading-snug',
  '--leading-base',
  '--leading-loose',
] as const;

// Letter-spacing (tracking) scale tokens — slice 3 of #165. Same shape
// contract as LEADING_TOKENS (single-mode, no dark override). Values are
// either `0` (the explicit `--tracking-normal` reset) or `±0.0Nem` — em so
// the spacing scales with font-size.
const TRACKING_TOKENS = [
  '--tracking-tighter',
  '--tracking-tight',
  '--tracking-normal',
  '--tracking-wide',
  '--tracking-wider',
  '--tracking-widest',
] as const;

// Border-radius scale tokens (slice 2 of #165). Same shape contract as
// TYPE_SCALE_TOKENS: single-mode (radius doesn't theme-vary), declared once
// in `:root` as a concrete `Npx` literal. The migration from 9 raw radius
// values (2/3/4/5/6/7/8/10/999 px plus the pre-existing 14px on `--r`)
// into 6 tokens is intentionally lossy — sites with 3/5/7px snap to the
// nearest token per the issue's rounding policy. The pre-existing `--r`
// alias is intentionally *not* listed here: it's a back-compat alias
// (`--r: var(--radius-xl)`) so consumers don't churn in this slice.
const RADIUS_TOKENS = [
  '--radius-xs',
  '--radius-sm',
  '--radius-md',
  '--radius-lg',
  '--radius-xl',
  '--radius-pill',
] as const;

// Status colors (slice 3b of #137). Positional-like: concrete oklch literals
// in `:root` AND in `[data-theme="dark"]`. The dark side carries an extra
// invariant — see DARK_STATUS_OKLCH below — locking in the L=74% standardize
// decision so a future "let me tweak the green a hair" can't silently undo it.
const STATUS_COLOR_TOKENS = [
  '--success',
  '--error',
] as const;

// Terminal traffic-light dot scale (slice 3b of #137). Positional pair of
// three tiers in each theme. We don't pin the exact values here (the dot
// hue/chroma is decorative, not a contrast-bound surface) but we do require
// both blocks declare all three tokens as oklch literals — the same shape
// contract as CONCRETE_SURFACE_TOKENS.
const TERM_DOT_TOKENS = [
  '--term-dot-light',
  '--term-dot-medium',
  '--term-dot-dark',
] as const;

// Font-family semantic aliases (slice 2 of #150). Same alias contract as
// `ALIAS_TOKENS` above — declared in `:root`, never re-declared in dark
// (font stacks don't theme-vary, so a dark override would just rot). The
// extra invariant is the *shape* of the value: each must resolve to one
// of the positional `--font-sans` / `--font-serif` / `--font-mono`
// tokens, not a hardcoded font stack. That keeps the positional tokens
// as the single source of truth for the actual font families.
const FONT_ALIAS_TOKENS = [
  '--font-display',
  '--font-numeric',
  '--font-code',
] as const;

// Semantic misc tokens added in slice 3d of #137. Each carries a concrete
// color literal in both `:root` and `[data-theme="dark"]` — same parity
// contract as POSITIONAL_TOKENS, but split out because the shape
// invariant differs: `--overlay-scrim` is intentionally `rgba(...)` (the
// dense modal dimmer has semantic intent — see the comment next to its
// declaration in calm.css), the others are oklch literals.
const SEMANTIC_MISC_TOKENS = [
  '--overlay-scrim',
  '--cal-event-waiting-bg',
  '--error-text',
  '--warn-border',
] as const;

// Z-index scale (slice 4 of #165). Six semantic stacking tiers declared once
// in `:root` as bare integer literals — no dark override (z-index is
// structural, not visual). The tokens establish a SEMANTIC ordering, not value
// preservation: every component selector reads through these, and stylelint
// bans raw `z-index: N` literals in component selectors (see
// `web/.stylelintrc.cjs` — `declaration-property-value-disallowed-list`).
//
// The order below is the contract — see Z_INDEX_TOKENS_ORDERED below for the
// ascending-value invariant.
const Z_INDEX_TOKENS = [
  '--z-base',
  '--z-raised',
  '--z-sticky',
  '--z-overlay',
  '--z-modal',
  '--z-toast',
] as const;

// ---------------------------------------------------------------------------
// Parse helpers
// ---------------------------------------------------------------------------

const CSS_PATH = resolve(dirname(fileURLToPath(import.meta.url)), 'calm.css');
const CSS = readFileSync(CSS_PATH, 'utf8');

/**
 * Slice the string between a block opener (e.g. `:root {`) and its matching
 * closing brace, counting nested `{ }` so a stray inner block doesn't end the
 * slice early. Returns the inner body (no outer braces).
 *
 * We can't trust a naive `indexOf('}')` — calm.css does have nested at-rules
 * (`@keyframes`, `@media`) elsewhere, and a future edit might land one inside
 * `:root` (unlikely, but cheap to guard against).
 */
function sliceBlock(source: string, opener: string): string {
  const start = source.indexOf(opener);
  if (start < 0) throw new Error(`block not found: ${opener}`);
  const bodyStart = start + opener.length;
  let depth = 1;
  for (let i = bodyStart; i < source.length; i++) {
    const ch = source[i];
    if (ch === '{') depth++;
    else if (ch === '}') {
      depth--;
      if (depth === 0) return source.slice(bodyStart, i);
    }
  }
  throw new Error(`unbalanced braces after: ${opener}`);
}

/**
 * Parse all `--foo: value;` declarations from a block body into a map. We
 * trim values but otherwise preserve them (whitespace inside `oklch(...)`
 * matters for the literal-vs-alias assertion later).
 */
function parseDecls(body: string): Map<string, string> {
  const out = new Map<string, string>();
  const re = /(--[a-z][a-z0-9-]*)\s*:\s*([^;]+);/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(body)) !== null) {
    out.set(m[1], m[2].trim());
  }
  return out;
}

const rootDecls = parseDecls(sliceBlock(CSS, ':root {'));
const darkDecls = parseDecls(sliceBlock(CSS, '[data-theme="dark"] {'));

// A `var(--something)` reference, with no trailing fallback. Aliases in
// calm.css are always the bare form — if a fallback ever shows up, we want a
// human to look at it.
const VAR_REF = /^var\(\s*(--[a-z][a-z0-9-]*)\s*\)$/;
// An `oklch(...)` literal. We don't validate the inside — that's what the
// browser's CSS parser is for.
const OKLCH_LITERAL = /^oklch\([^)]+\)$/;

// ---------------------------------------------------------------------------
// (a) Aliases must be `var()` references, concrete tokens must be literals.
// ---------------------------------------------------------------------------
//
// "Don't hardcode a value in a semantic alias" is the contract from #137.
// Equivalent: aliases stay as `var(--positional)`, and concrete tokens stay
// as `oklch(...)`. No mixing.

describe('calm.css token graph: alias vs. concrete shape', () => {
  for (const name of ALIAS_TOKENS) {
    it(`${name} is declared in :root as a var() reference`, () => {
      const value = rootDecls.get(name);
      expect(value, `${name} missing from :root`).toBeDefined();
      expect(
        value,
        `${name} should be 'var(--something)' (a semantic alias), got: ${value}`,
      ).toMatch(VAR_REF);
    });
  }

  for (const name of CONCRETE_SURFACE_TOKENS) {
    it(`${name} is declared in :root as an oklch() literal`, () => {
      const value = rootDecls.get(name);
      expect(value, `${name} missing from :root`).toBeDefined();
      expect(
        value,
        `${name} is a concrete surface — should be 'oklch(...)', not a var(). Got: ${value}`,
      ).toMatch(OKLCH_LITERAL);
    });
  }
});

// ---------------------------------------------------------------------------
// (b) Positional tokens have light + dark parity.
// ---------------------------------------------------------------------------

describe('calm.css token graph: positional tokens', () => {
  for (const name of POSITIONAL_TOKENS) {
    it(`${name} is declared in both :root and [data-theme="dark"]`, () => {
      const inLight = rootDecls.has(name);
      const inDark = darkDecls.has(name);
      // Report both sides on failure so the fixup is obvious.
      expect(
        { token: name, light: inLight, dark: inDark },
        `${name} must be defined in both blocks`,
      ).toEqual({ token: name, light: true, dark: true });
    });
  }
});

// ---------------------------------------------------------------------------
// (c) Concrete surface tokens have light + dark parity, both as literals.
// ---------------------------------------------------------------------------

describe('calm.css token graph: concrete surface tokens', () => {
  for (const name of CONCRETE_SURFACE_TOKENS) {
    it(`${name} has matching oklch() literals in :root and dark`, () => {
      const light = rootDecls.get(name);
      const dark = darkDecls.get(name);
      expect(light, `${name} missing from :root`).toBeDefined();
      expect(dark, `${name} missing from [data-theme="dark"]`).toBeDefined();
      expect(
        light,
        `${name} light value should be an oklch() literal, got: ${light}`,
      ).toMatch(OKLCH_LITERAL);
      expect(
        dark,
        `${name} dark value should be an oklch() literal (not a var alias), got: ${dark}`,
      ).toMatch(OKLCH_LITERAL);
    });
  }
});

// (c2) Prose / code surface tokens (slice 3c of #137): same shape contract
// as CONCRETE_SURFACE_TOKENS — oklch literal in both blocks, theme-varying.
// ---------------------------------------------------------------------------

describe('calm.css token graph: prose / code surface tokens', () => {
  for (const name of PROSE_SURFACE_TOKENS) {
    it(`${name} has matching oklch() literals in :root and dark`, () => {
      const light = rootDecls.get(name);
      const dark = darkDecls.get(name);
      expect(light, `${name} missing from :root`).toBeDefined();
      expect(dark, `${name} missing from [data-theme="dark"]`).toBeDefined();
      expect(
        light,
        `${name} light value should be an oklch() literal, got: ${light}`,
      ).toMatch(OKLCH_LITERAL);
      expect(
        dark,
        `${name} dark value should be an oklch() literal (not a var alias), got: ${dark}`,
      ).toMatch(OKLCH_LITERAL);
    });
  }
});

// ---------------------------------------------------------------------------
// (c3) Diff line palette tokens (slice 3c of #137): bg + text pairs, each
// theme-varying, each an oklch literal.
// ---------------------------------------------------------------------------

describe('calm.css token graph: diff color tokens', () => {
  for (const name of DIFF_COLOR_TOKENS) {
    it(`${name} has matching oklch() literals in :root and dark`, () => {
      const light = rootDecls.get(name);
      const dark = darkDecls.get(name);
      expect(light, `${name} missing from :root`).toBeDefined();
      expect(dark, `${name} missing from [data-theme="dark"]`).toBeDefined();
      expect(
        light,
        `${name} light value should be an oklch() literal, got: ${light}`,
      ).toMatch(OKLCH_LITERAL);
      expect(
        dark,
        `${name} dark value should be an oklch() literal (not a var alias), got: ${dark}`,
      ).toMatch(OKLCH_LITERAL);
    });
  }
});

// ---------------------------------------------------------------------------
// (c′) Overlay tokens have light + dark parity, both as oklch() literals.
// ---------------------------------------------------------------------------
//
// Same contract as concrete surface tokens — declared in both `:root` and
// `[data-theme="dark"]`, with concrete oklch values on each side. The light
// literal uses `oklch(0% 0 0 / 0.0X)` (black at low alpha for cold paper);
// the dark literal uses `oklch(100% 0 0 / 0.0X)` (white at low alpha for
// warm graphite). We don't assert the inversion in regex form — that would
// over-fit — but the structural shape (both are oklch literals) is the
// gate that catches "oh I aliased it to a var() in dark" regressions.

describe('calm.css token graph: overlay tokens', () => {
  for (const name of OVERLAY_TOKENS) {
    it(`${name} has matching oklch() literals in :root and dark`, () => {
      const light = rootDecls.get(name);
      const dark = darkDecls.get(name);
      expect(light, `${name} missing from :root`).toBeDefined();
      expect(dark, `${name} missing from [data-theme="dark"]`).toBeDefined();
      expect(
        light,
        `${name} light value should be an oklch() literal, got: ${light}`,
      ).toMatch(OKLCH_LITERAL);
      expect(
        dark,
        `${name} dark value should be an oklch() literal (not a var alias), got: ${dark}`,
      ).toMatch(OKLCH_LITERAL);
    });
  }
});

// ---------------------------------------------------------------------------
// (d) Alias tokens MUST NOT have a dark override.
// ---------------------------------------------------------------------------
//
// Overriding an alias in dark defeats the cascade: the alias resolves to a
// positional token that already swaps. A dark override here is almost always
// a copy-paste bug — flag it loudly.

describe('calm.css token graph: alias tokens have no dark override', () => {
  for (const name of ALIAS_TOKENS) {
    it(`${name} is NOT redeclared in [data-theme="dark"]`, () => {
      expect(
        darkDecls.has(name),
        `${name} is a semantic alias; dark override would shadow the cascade. Remove it from [data-theme="dark"].`,
      ).toBe(false);
    });
  }
});

// ---------------------------------------------------------------------------
// (e) Type-scale tokens: defined in :root, concrete px literals, no dark.
// ---------------------------------------------------------------------------
//
// Mirrors the contract for concrete surface tokens but with two twists:
//  - The literal is a numeric `Npx` (or `N.Npx`) — not oklch.
//  - The token MUST NOT have a dark override; type scale is shape, not color.

const PX_LITERAL = /^\d+(\.\d+)?px$/;

describe('calm.css token graph: type-scale tokens', () => {
  for (const name of TYPE_SCALE_TOKENS) {
    it(`${name} is declared in :root as a numeric px literal`, () => {
      const value = rootDecls.get(name);
      expect(value, `${name} missing from :root`).toBeDefined();
      expect(
        value,
        `${name} should be a concrete 'Npx' literal (type scale doesn't theme-vary), got: ${value}`,
      ).toMatch(PX_LITERAL);
    });

    it(`${name} is NOT redeclared in [data-theme="dark"]`, () => {
      expect(
        darkDecls.has(name),
        `${name} is a type-scale token; type scale is shape, not color — no dark override.`,
      ).toBe(false);
    });
  }
});

// ---------------------------------------------------------------------------
// (e1b) Border-radius scale tokens (slice 2 of #165).
// ---------------------------------------------------------------------------
//
// Same shape contract as TYPE_SCALE_TOKENS: concrete `Npx` literal in `:root`,
// no dark override. Radius is part of the design system's shape vocabulary
// (like type scale and `--r`), not its color vocabulary — overriding it in
// dark would break the cascade and add noise without benefit.

describe('calm.css token graph: radius-scale tokens', () => {
  for (const name of RADIUS_TOKENS) {
    it(`${name} is declared in :root as a numeric px literal`, () => {
      const value = rootDecls.get(name);
      expect(value, `${name} missing from :root`).toBeDefined();
      expect(
        value,
        `${name} should be a concrete 'Npx' literal (radius scale doesn't theme-vary), got: ${value}`,
      ).toMatch(PX_LITERAL);
    });

    it(`${name} is NOT redeclared in [data-theme="dark"]`, () => {
      expect(
        darkDecls.has(name),
        `${name} is a radius-scale token; radius is shape, not color — no dark override.`,
      ).toBe(false);
    });
  }
});

// ---------------------------------------------------------------------------
// (e1.5) Leading + tracking scale tokens (slice 3 of #165).
// ---------------------------------------------------------------------------
//
// Same single-mode shape contract as the type scale (declared in `:root`,
// never in `[data-theme="dark"]`). The value-shape gate differs:
//   - Leading values are unitless (`1`, `1.15`, …) so the ratio composes
//     against any inherited font-size at the call site.
//   - Tracking values are either `0` (the explicit `--tracking-normal`
//     reset, distinct from CSS `normal` so the linter can disallow the
//     bare keyword) or `±0.0Nem` so the spacing scales with font-size.
//
// Like the type scale, the migration from ~10 leading and ~9 tracking
// distinct literals into 5 + 6 tiers is intentionally lossy; the rounding
// policy lives in the #165 slice 3 body, not here.

const UNITLESS_LITERAL = /^\d+(\.\d+)?$/;
const TRACKING_LITERAL = /^(0|-?0\.\d+em)$/;

describe('calm.css token graph: leading scale tokens', () => {
  for (const name of LEADING_TOKENS) {
    it(`${name} is declared in :root as a unitless number`, () => {
      const value = rootDecls.get(name);
      expect(value, `${name} missing from :root`).toBeDefined();
      expect(
        value,
        `${name} should be a unitless number (line-height composes against inherited font-size), got: ${value}`,
      ).toMatch(UNITLESS_LITERAL);
    });

    it(`${name} is NOT redeclared in [data-theme="dark"]`, () => {
      expect(
        darkDecls.has(name),
        `${name} is a leading token; leading is shape, not color — no dark override.`,
      ).toBe(false);
    });
  }
});

describe('calm.css token graph: tracking scale tokens', () => {
  for (const name of TRACKING_TOKENS) {
    it(`${name} is declared in :root as 0 or ±0.0Nem`, () => {
      const value = rootDecls.get(name);
      expect(value, `${name} missing from :root`).toBeDefined();
      expect(
        value,
        `${name} should be '0' or '±0.0Nem' (em so tracking scales with font-size), got: ${value}`,
      ).toMatch(TRACKING_LITERAL);
    });

    it(`${name} is NOT redeclared in [data-theme="dark"]`, () => {
      expect(
        darkDecls.has(name),
        `${name} is a tracking token; tracking is shape, not color — no dark override.`,
      ).toBe(false);
    });
  }
});

// ---------------------------------------------------------------------------
// (e2) Status color tokens (slice 3b of #137).
// ---------------------------------------------------------------------------
//
// `--success` / `--error` are positional in shape (literal in both blocks,
// theme-vary), but they carry an extra contract: the dark side standardizes
// at L=74% across the family. Pre-consolidation we had three values (72/74/78)
// scattered across selectors; this regex pins the design call so a "let me
// nudge the green" two months from now has to come back and update the test.

const DARK_STATUS_OKLCH = /^oklch\(74% 0\.14 (145|25)\)$/;

describe('calm.css token graph: status color tokens', () => {
  for (const name of STATUS_COLOR_TOKENS) {
    it(`${name} is declared in :root as an oklch() literal`, () => {
      const value = rootDecls.get(name);
      expect(value, `${name} missing from :root`).toBeDefined();
      expect(
        value,
        `${name} should be a concrete oklch() literal in :root, got: ${value}`,
      ).toMatch(OKLCH_LITERAL);
    });

    it(`${name} is declared in [data-theme="dark"] at L=74% (standardized)`, () => {
      const value = darkDecls.get(name);
      expect(value, `${name} missing from [data-theme="dark"]`).toBeDefined();
      // Pin the standardization decision: dark-mode status colors collapse
      // to L=74% / chroma 0.14, hue 145 (success) or 25 (error). If you're
      // here to relax this, please link the design discussion in the PR.
      expect(
        value,
        `${name} dark value must be 'oklch(74% 0.14 H)' (the slice 3b standardization). Got: ${value}`,
      ).toMatch(DARK_STATUS_OKLCH);
    });
  }
});

// ---------------------------------------------------------------------------
// (e3) Terminal-dot scale tokens (slice 3b of #137).
// ---------------------------------------------------------------------------
//
// Same shape contract as concrete surface tokens — literal in both blocks.
// We don't pin the exact L values: the dot is a decorative traffic-light,
// not a contrast-bound surface, and a future re-tune is acceptable as long
// as the three-tier vocabulary stays intact.

describe('calm.css token graph: terminal-dot scale tokens', () => {
  for (const name of TERM_DOT_TOKENS) {
    it(`${name} has matching oklch() literals in :root and dark`, () => {
      const light = rootDecls.get(name);
      const dark = darkDecls.get(name);
      expect(light, `${name} missing from :root`).toBeDefined();
      expect(dark, `${name} missing from [data-theme="dark"]`).toBeDefined();
      expect(
        light,
        `${name} light value should be an oklch() literal, got: ${light}`,
      ).toMatch(OKLCH_LITERAL);
      expect(
        dark,
        `${name} dark value should be an oklch() literal, got: ${dark}`,
      ).toMatch(OKLCH_LITERAL);
    });
  }
});

// ---------------------------------------------------------------------------
// (f) Font-family aliases (slice 2 of #150).
// ---------------------------------------------------------------------------
//
// `--font-display` / `--font-numeric` / `--font-code` are semantic aliases
// over the positional `--font-sans` / `--font-serif` / `--font-mono`. Three
// invariants:
//   1. Declared in `:root`.
//   2. Value is a bare `var(--font-{sans,serif,mono})` reference — never a
//      hardcoded font stack. The positional tokens stay the one place that
//      pins concrete font families.
//   3. NOT redeclared in `[data-theme="dark"]` — fonts don't theme-vary,
//      and an override would defeat the cascade.

// Stricter than VAR_REF: must point at one of the three positional font
// tokens specifically (not at some other alias).
const FONT_VAR_REF = /^var\(--font-(sans|serif|mono)\)$/;

describe('calm.css token graph: font-family aliases', () => {
  for (const name of FONT_ALIAS_TOKENS) {
    it(`${name} is declared in :root as a var(--font-{sans|serif|mono}) reference`, () => {
      const value = rootDecls.get(name);
      expect(value, `${name} missing from :root`).toBeDefined();
      expect(
        value,
        `${name} should alias one of --font-sans/--font-serif/--font-mono (no hardcoded font stacks). Got: ${value}`,
      ).toMatch(FONT_VAR_REF);
    });

    it(`${name} is NOT redeclared in [data-theme="dark"]`, () => {
      expect(
        darkDecls.has(name),
        `${name} is a font alias; font stacks don't theme-vary. Remove from [data-theme="dark"].`,
      ).toBe(false);
    });
  }
});

// ---------------------------------------------------------------------------
// (f2) Semantic misc tokens (slice 3d of #137): scrim, cal-waiting bg,
// error text, warn border.
// ---------------------------------------------------------------------------
//
// Same light+dark parity contract as POSITIONAL_TOKENS, but the value
// shape differs by token:
//   - `--overlay-scrim` is deliberately `rgba(...)` — the dense modal
//     dimmer has semantic intent ("dim and slightly cool the background")
//     that's awkward to express as a pure oklch with alpha. The
//     declaration sits inside the block-disable from #149 so stylelint's
//     ban on rgba() in component selectors doesn't fire; consumers read
//     it through `var()`. This is the one deliberate exception to the
//     "oklch literals only" rule for concrete tokens.
//   - The others are oklch literals like the rest of the concrete tokens.

const RGBA_LITERAL = /^rgba\(.+\)$/;

describe('calm.css token graph: semantic misc tokens (#137 slice 3d)', () => {
  for (const name of SEMANTIC_MISC_TOKENS) {
    it(`${name} is declared in both :root and [data-theme="dark"]`, () => {
      const inLight = rootDecls.has(name);
      const inDark = darkDecls.has(name);
      expect(
        { token: name, light: inLight, dark: inDark },
        `${name} must be defined in both blocks`,
      ).toEqual({ token: name, light: true, dark: true });
    });
  }

  // --overlay-scrim: rgba() form is deliberate (see header comment above).
  for (const block of [
    { name: ':root', decls: rootDecls },
    { name: '[data-theme="dark"]', decls: darkDecls },
  ]) {
    it(`--overlay-scrim in ${block.name} is an rgba() literal (deliberate exception)`, () => {
      const value = block.decls.get('--overlay-scrim');
      expect(value, `--overlay-scrim missing from ${block.name}`).toBeDefined();
      expect(
        value,
        `--overlay-scrim is intentionally rgba() — dense modal dimmer with semantic intent. Got: ${value}`,
      ).toMatch(RGBA_LITERAL);
    });
  }

  // Other semantic misc tokens follow the standard oklch-literal contract.
  const OKLCH_ONLY = SEMANTIC_MISC_TOKENS.filter((n) => n !== '--overlay-scrim');
  for (const name of OKLCH_ONLY) {
    for (const block of [
      { name: ':root', decls: rootDecls },
      { name: '[data-theme="dark"]', decls: darkDecls },
    ]) {
      it(`${name} in ${block.name} is an oklch() literal`, () => {
        const value = block.decls.get(name);
        expect(value, `${name} missing from ${block.name}`).toBeDefined();
        expect(
          value,
          `${name} should be an oklch() literal, got: ${value}`,
        ).toMatch(OKLCH_LITERAL);
      });
    }
  }
});

// ---------------------------------------------------------------------------
// (f3) Z-index scale tokens (slice 4 of #165).
// ---------------------------------------------------------------------------
//
// Three invariants:
//   1. Each token is declared in `:root` as a bare integer literal (the
//      scale is value-based; no `var()` chains, no `calc()`).
//   2. No dark override — z-index is structural, not visual.
//   3. The values are strictly ascending in the declared order:
//        --z-base < --z-raised < --z-sticky < --z-overlay < --z-modal < --z-toast
//      This converts "the ordering is semantic" into a contract, so a
//      future "let me bump --z-sticky to 50" can't silently invert tiers
//      without the test failing.

const INT_LITERAL = /^\d+$/;

describe('calm.css token graph: z-index scale tokens (#165 slice 4)', () => {
  for (const name of Z_INDEX_TOKENS) {
    it(`${name} is declared in :root as a bare integer literal`, () => {
      const value = rootDecls.get(name);
      expect(value, `${name} missing from :root`).toBeDefined();
      expect(
        value,
        `${name} should be a bare integer (the z-scale is value-based, no var()/calc()). Got: ${value}`,
      ).toMatch(INT_LITERAL);
    });

    it(`${name} is NOT redeclared in [data-theme="dark"]`, () => {
      expect(
        darkDecls.has(name),
        `${name} is a z-index token; z-index is structural, not visual — no dark override.`,
      ).toBe(false);
    });
  }

  it('values follow strictly ascending order base < raised < sticky < overlay < modal < toast', () => {
    const values = Z_INDEX_TOKENS.map((name) => {
      const raw = rootDecls.get(name);
      expect(raw, `${name} missing from :root`).toBeDefined();
      return { name, n: Number(raw) };
    });
    for (let i = 1; i < values.length; i++) {
      const prev = values[i - 1];
      const curr = values[i];
      expect(
        curr.n > prev.n,
        `z-scale must strictly ascend: ${prev.name} (${prev.n}) must be < ${curr.name} (${curr.n})`,
      ).toBe(true);
    }
  });
});

// ---------------------------------------------------------------------------
// (g) Soft drift detector: any `font-size: Npx` literal outside the token
// blocks is fair game for migration (or a deliberate exception that should
// be called out in review).
// ---------------------------------------------------------------------------

// Shared helper for the soft drift detectors below. Replaces the
// `:root { … }` and `[data-theme="dark"] { … }` blocks with blank lines of
// equal line count, so when we regex-scan the result the matched offsets
// still correspond to the original file's line numbers in `console.warn`.
function maskedSourceWithoutTokenBlocks(): string {
  const rootStart = CSS.indexOf(':root {');
  const rootBody = sliceBlock(CSS, ':root {');
  const rootEnd = CSS.indexOf(rootBody, rootStart) + rootBody.length + 1;
  const darkStart = CSS.indexOf('[data-theme="dark"] {');
  const darkBody = sliceBlock(CSS, '[data-theme="dark"] {');
  const darkEnd = CSS.indexOf(darkBody, darkStart) + darkBody.length + 1;
  const lineCountIn = (s: string) => (s.match(/\n/g) || []).length;
  const blank = (n: number) => '\n'.repeat(n);
  return (
    CSS.slice(0, rootStart) +
    blank(lineCountIn(CSS.slice(rootStart, rootEnd))) +
    CSS.slice(rootEnd, darkStart) +
    blank(lineCountIn(CSS.slice(darkStart, darkEnd))) +
    CSS.slice(darkEnd)
  );
}

describe('calm.css type-scale: no raw font-size literals outside :root', () => {
  it('logs any remaining font-size px literal in component selectors (informational)', () => {
    // Skip the token-definition blocks (`:root { … }` and the dark block).
    // Anything else is a component selector that should be reading through a
    // var(--text-*) token. We surface this as a warning today; if drift
    // appears in CI noise we can promote to a hard failure later.
    const masked = maskedSourceWithoutTokenBlocks();

    const hits: string[] = [];
    const re = /font-size:\s*\d+(?:\.\d+)?px/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(masked)) !== null) {
      const line = masked.slice(0, m.index).split('\n').length;
      hits.push(`calm.css:${line}: ${m[0]}`);
    }
    if (hits.length > 0) {
      // eslint-disable-next-line no-console
      console.warn(
        `[calm-tokens] raw font-size px literals outside :root/dark token blocks (migrate to var(--text-*) per #150):\n  ${hits.join('\n  ')}`,
      );
    }
    expect(true).toBe(true);
  });
});

// (g2) Soft drift detector for leading + tracking literals (slice 3 of #165).
// Same masking pattern as font-size above. Hard enforcement lives in the
// stylelint config (`declaration-property-value-disallowed-list`); this
// detector is the human-readable "here are the lines" report for whichever
// future slice promotes the gate to a CI failure.
describe('calm.css leading scale: no raw line-height literals outside :root', () => {
  it('logs any remaining line-height numeric literal in component selectors (informational)', () => {
    const masked = maskedSourceWithoutTokenBlocks();
    const hits: string[] = [];
    // Match bare numeric forms only (`1`, `1.55`). `inherit`, `normal`, and
    // `var(--leading-*)` are intentionally excused.
    const re = /line-height:\s*\d+(?:\.\d+)?/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(masked)) !== null) {
      const line = masked.slice(0, m.index).split('\n').length;
      hits.push(`calm.css:${line}: ${m[0]}`);
    }
    if (hits.length > 0) {
      // eslint-disable-next-line no-console
      console.warn(
        `[calm-tokens] raw line-height literals outside :root/dark token blocks (migrate to var(--leading-*) per #165):\n  ${hits.join('\n  ')}`,
      );
    }
    expect(true).toBe(true);
  });
});

describe('calm.css tracking scale: no raw letter-spacing literals outside :root', () => {
  it('logs any remaining letter-spacing numeric literal in component selectors (informational)', () => {
    const masked = maskedSourceWithoutTokenBlocks();
    const hits: string[] = [];
    // Match `±N.Nem`/`±Npx`/`±Nrem` — bare numeric forms. `inherit` and
    // `var(--tracking-*)` are intentionally excused. `normal` is also
    // excused here (the migration left `--tracking-normal` for everything
    // except the one inherited-tracking reset); the stylelint gate enforces
    // the numeric ban directly.
    const re = /letter-spacing:\s*-?\d+(?:\.\d+)?(?:em|rem|px)/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(masked)) !== null) {
      const line = masked.slice(0, m.index).split('\n').length;
      hits.push(`calm.css:${line}: ${m[0]}`);
    }
    if (hits.length > 0) {
      // eslint-disable-next-line no-console
      console.warn(
        `[calm-tokens] raw letter-spacing literals outside :root/dark token blocks (migrate to var(--tracking-*) per #165):\n  ${hits.join('\n  ')}`,
      );
    }
    expect(true).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// (g2) Soft drift detector for raw `border-radius` literals outside :root.
// ---------------------------------------------------------------------------
//
// Mirror of the font-size drift detector above (#150 / slice 1) but for the
// radius scale introduced in slice 2 of #165. Component selectors should be
// reading `border-radius` through one of `var(--radius-*)` (or the legacy
// `var(--r)` alias, which itself points at `--radius-xl`). Anything else is
// drift and should be migrated or have an inline disable with a reason.
//
// Soft today (informational `console.warn`); the stylelint rule in
// `.stylelintrc.cjs` is the hard gate. The test exists as a second
// reviewer — it surfaces drift in `npm test` output even before lint runs,
// and includes line numbers so the migration is one-shot.

describe('calm.css radius-scale: no raw border-radius literals outside :root', () => {
  it('logs any remaining border-radius numeric literal in component selectors (informational)', () => {
    const rootStart = CSS.indexOf(':root {');
    const rootBody = sliceBlock(CSS, ':root {');
    const rootEnd = CSS.indexOf(rootBody, rootStart) + rootBody.length + 1;
    const darkStart = CSS.indexOf('[data-theme="dark"] {');
    const darkBody = sliceBlock(CSS, '[data-theme="dark"] {');
    const darkEnd = CSS.indexOf(darkBody, darkStart) + darkBody.length + 1;

    const lineCountIn = (s: string) => (s.match(/\n/g) || []).length;
    const blank = (n: number) => '\n'.repeat(n);
    const masked =
      CSS.slice(0, rootStart) +
      blank(lineCountIn(CSS.slice(rootStart, rootEnd))) +
      CSS.slice(rootEnd, darkStart) +
      blank(lineCountIn(CSS.slice(darkStart, darkEnd))) +
      CSS.slice(darkEnd);

    const hits: string[] = [];
    // Match `border-radius: Npx` (and `border-*-radius` long-hands). We don't
    // match `50%` here — the few intentional-percentage uses have been
    // migrated to `--radius-pill` already, and the simpler `\d+px` form is
    // what we actually want to catch (drift would re-introduce px literals).
    const re = /border(?:-(?:top|bottom|left|right))?(?:-(?:top|bottom|left|right))?-radius:\s*\d+(?:\.\d+)?px/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(masked)) !== null) {
      const line = masked.slice(0, m.index).split('\n').length;
      hits.push(`calm.css:${line}: ${m[0]}`);
    }
    if (hits.length > 0) {
      // eslint-disable-next-line no-console
      console.warn(
        `[calm-tokens] raw border-radius px literals outside :root/dark token blocks (migrate to var(--radius-*) per #165):\n  ${hits.join('\n  ')}`,
      );
    }
    expect(true).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// (h) Orphan detection — soft warning, no failure.
// ---------------------------------------------------------------------------
//
// If a token is declared in `:root` but never referenced by a `var(--name)`
// elsewhere in the file, it's dead code. Today we just log — promote to a
// hard failure once token churn settles and we trust the inventory.

// ---------------------------------------------------------------------------
// (f) JS↔CSS mono-stack drift — `MONO_STACK` constant must match `--font-mono`.
// ---------------------------------------------------------------------------
//
// `font-stack.ts:MONO_STACK` is the source of truth for JS consumers that need
// a font-family string (xterm.js, fallback inline styles in UnknownCard, etc.).
// `--font-mono` in `:root` is the source of truth for CSS. The two must agree
// byte-for-byte — otherwise the terminal and the rest of the UI silently fall
// back to different system mono faces, which we'd never notice in review.
//
// Same "vocabulary becomes contract" pattern as the rest of this file: if you
// change one, update the other or CI fails. The assertion is intentionally a
// trimmed string compare — no fuzzy normalization, because subtle differences
// like quoting `"SF Mono"` vs `SF Mono` actually matter to the browser font
// resolver and we want them caught.
//
// #150 slice 3.
describe('calm.css ↔ font-stack.ts: mono stack drift', () => {
  it('--font-mono matches MONO_STACK byte-for-byte', () => {
    const fontMonoValue = rootDecls.get('--font-mono');
    expect(fontMonoValue, '--font-mono missing from :root').toBeDefined();
    expect(fontMonoValue).toBe(MONO_STACK);
  });
});

describe('calm.css token graph: orphan detection (soft)', () => {
  it('logs tokens with zero consumers (informational)', () => {
    const orphans: string[] = [];
    for (const [name] of rootDecls) {
      // Skip the few tokens that are intentionally used by name from JS or
      // are non-color shape primitives — checking those is out of scope for
      // a regex-based scan and would generate noise.
      if (name === '--r' || name === '--shadow') continue;
      if (name.startsWith('--font-')) continue;
      // Look for `var(--name)` anywhere in the file (not just in the same
      // block). The regex bounds the name to avoid `--text` matching
      // `--text-2` etc.
      const consumerRe = new RegExp(`var\\(\\s*${name}(?![a-z0-9-])`);
      if (!consumerRe.test(CSS)) orphans.push(name);
    }
    if (orphans.length > 0) {
      // eslint-disable-next-line no-console
      console.warn(
        `[calm-tokens] tokens declared in :root with zero var() consumers: ${orphans.join(', ')}. ` +
          `Promote to a hard failure once #142 token churn settles.`,
      );
    }
    // Soft assertion: always passes. Re-state for the reader.
    expect(true).toBe(true);
  });
});
