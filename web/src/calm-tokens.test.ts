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
// (e) Orphan detection — soft warning, no failure.
// ---------------------------------------------------------------------------
//
// If a token is declared in `:root` but never referenced by a `var(--name)`
// elsewhere in the file, it's dead code. Today we just log — promote to a
// hard failure once token churn settles and we trust the inventory.

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
