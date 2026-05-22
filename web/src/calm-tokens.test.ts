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

// ---------------------------------------------------------------------------
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
// (g) Soft drift detector: any `font-size: Npx` literal outside the token
// blocks is fair game for migration (or a deliberate exception that should
// be called out in review).
// ---------------------------------------------------------------------------

describe('calm.css type-scale: no raw font-size literals outside :root', () => {
  it('logs any remaining font-size px literal in component selectors (informational)', () => {
    // Skip the token-definition blocks (`:root { … }` and the dark block).
    // Anything else is a component selector that should be reading through a
    // var(--text-*) token. We surface this as a warning today; if drift
    // appears in CI noise we can promote to a hard failure later.
    const rootStart = CSS.indexOf(':root {');
    const rootBody = sliceBlock(CSS, ':root {');
    const rootEnd = CSS.indexOf(rootBody, rootStart) + rootBody.length + 1;
    const darkStart = CSS.indexOf('[data-theme="dark"] {');
    const darkBody = sliceBlock(CSS, '[data-theme="dark"] {');
    const darkEnd = CSS.indexOf(darkBody, darkStart) + darkBody.length + 1;

    // Build the source-minus-token-blocks string. We replace the token
    // blocks with newlines of equal line count so line numbers in the
    // warning still line up with the original file.
    const lineCountIn = (s: string) => (s.match(/\n/g) || []).length;
    const blank = (n: number) => '\n'.repeat(n);
    const masked =
      CSS.slice(0, rootStart) +
      blank(lineCountIn(CSS.slice(rootStart, rootEnd))) +
      CSS.slice(rootEnd, darkStart) +
      blank(lineCountIn(CSS.slice(darkStart, darkEnd))) +
      CSS.slice(darkEnd);

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
