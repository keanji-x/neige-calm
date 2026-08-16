// @vitest-environment node

/// <reference types="node" />

import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import { MONO_STACK } from './font-stack.js';

const css = readFileSync(new URL('./tokens.css', import.meta.url), 'utf8');
const rootSource = css.match(/:root\s*\{(?<body>[\s\S]*?)\}/)?.groups?.body ?? '';
const darkSource =
  css.match(/\[data-theme=["']dark["']\]\s*\{(?<body>[\s\S]*?)\}/)?.groups?.body ?? '';

function declarations(source: string): ReadonlyMap<string, string> {
  return new Map(
    [...source.matchAll(/(?<name>--[a-z0-9-]+)\s*:\s*(?<value>[^;]+);/g)].map((match) => [
      match.groups?.name ?? '',
      match.groups?.value.trim() ?? '',
    ]),
  );
}

const root = declarations(rootSource);
const dark = declarations(darkSource);

const POSITIONAL = [
  '--bg', '--paper', '--hairline', '--hairline-strong', '--text', '--text-2', '--text-3',
  '--text-4', '--accent', '--accent-soft', '--warn', '--warn-soft',
] as const;
const CONCRETE_SURFACES = [
  '--surface-rail', '--surface-card', '--surface-chip', '--surface-chip-focus', '--surface-panel-head',
] as const;
/* `--surface-said` sits here rather than with the concrete surfaces for the
   same reason `--surface-code` does: it is an alpha tint over whatever it lands
   on, not a rank on the elevation ladder. */
const PROSE_SURFACES = ['--surface-terminal', '--surface-code', '--surface-said'] as const;
const OVERLAYS = [
  '--overlay-hover-faint', '--overlay-hover', '--overlay-hover-strong', '--overlay-active',
] as const;
const TYPE_SCALE = [
  '--text-xs', '--text-base', '--text-md', '--text-lg', '--text-xl',
] as const;
const LEADING = [
  '--leading-none', '--leading-tight', '--leading-snug', '--leading-base', '--leading-loose',
] as const;
const TRACKING = [
  '--tracking-tighter', '--tracking-tight', '--tracking-normal', '--tracking-wide',
  '--tracking-wider', '--tracking-widest',
] as const;
const RADIUS = [
  '--radius-sm', '--radius-md', '--radius-lg', '--radius-pill',
] as const;
const SPACING = [
  '--space-0', '--space-px', '--space-1', '--space-2', '--space-3', '--space-4', '--space-5',
  '--space-6', '--space-7', '--space-8', '--space-9', '--space-10', '--space-11', '--space-12',
] as const;
const MOTION = [
  '--motion-instant', '--motion-quick', '--motion-snappy', '--motion-medium', '--motion-slow',
  '--motion-pulse',
] as const;
const STATUS = ['--success', '--error'] as const;
const FONT_ALIASES = ['--font-display', '--font-numeric', '--font-code'] as const;
const MISC = [
  '--overlay-scrim', '--error-text', '--warn-border',
  '--warn-text', '--success-text', '--error-soft', '--error-border',
] as const;
// Every MISC member except the scrim, which is the deliberate rgba exception.
const MISC_OKLCH = MISC.filter((name) => name !== '--overlay-scrim');
const Z_INDEX = [
  '--z-base', '--z-raised', '--z-sticky', '--z-overlay', '--z-modal', '--z-toast',
] as const;
const BOX_SCALE = [
  '--row-h-sm', '--row-h', '--row-h-lg',
  '--control-h-sm', '--control-h', '--control-h-lg',
  '--rail-w', '--rail-w-collapsed', '--panel-w', '--drawer-w', '--header-baseline',
  '--measure-prose', '--measure-form', '--measure-page', '--measure-board',
  '--measure-list', '--measure-doc',
  '--slot-h', '--rule-h', '--dot-sm', '--dot-md',
  '--glyph-sm', '--glyph', '--menu-w-min', '--menu-w-max',
] as const;
const WEIGHTS = ['--weight-normal', '--weight-medium', '--weight-semibold'] as const;
const COVE_IDENTITY = [
  '--cove-1', '--cove-2', '--cove-3', '--cove-4',
  '--cove-5', '--cove-6', '--cove-7', '--cove-8',
] as const;
const SHADOW = ['--shadow-float'] as const;
// This alias must resolve in both themes (§0.1 #10).
const THEMED_ALIASES = ['--text-on-accent'] as const;
const INVENTORY = [
  ...POSITIONAL, ...CONCRETE_SURFACES, ...PROSE_SURFACES, ...OVERLAYS,
  ...TYPE_SCALE, ...LEADING, ...TRACKING, ...RADIUS, ...SPACING, ...MOTION, ...STATUS, ...FONT_ALIASES,
  ...MISC, ...Z_INDEX,
  ...BOX_SCALE, ...WEIGHTS, ...COVE_IDENTITY, ...SHADOW, ...THEMED_ALIASES,
] as const;

describe('styles/tokens inventory contract', () => {
  it('keeps the independently pinned token inventory exact', () => {
    const actual = [...root.keys(), ...dark.keys()].filter((name, index, all) => all.indexOf(name) === index);
    expect(actual.sort()).toEqual([...INVENTORY, '--font-sans', '--font-serif', '--font-mono'].sort());
  });
});

describe('styles/tokens themed color contracts', () => {
  it.each(POSITIONAL)('%s has light/dark parity', (name) => {
    expect(root.has(name)).toBe(true);
    expect(dark.has(name)).toBe(true);
  });

  describe('concrete surfaces', () => {
    it.each(CONCRETE_SURFACES)('%s is a concrete oklch literal in both themes', (name) => {
      expect(root.get(name)).toMatch(/^oklch\([^)]*\)$/);
      expect(dark.get(name)).toMatch(/^oklch\([^)]*\)$/);
    });
  });

  describe('prose surfaces', () => {
    it.each(PROSE_SURFACES)('%s is a concrete oklch literal in both themes', (name) => {
      expect(root.get(name)).toMatch(/^oklch\([^)]*\)$/);
      expect(dark.get(name)).toMatch(/^oklch\([^)]*\)$/);
    });
  });

  describe('overlays', () => {
    it.each(OVERLAYS)('%s is a concrete oklch literal in both themes', (name) => {
      expect(root.get(name)).toMatch(/^oklch\([^)]*\)$/);
      expect(dark.get(name)).toMatch(/^oklch\([^)]*\)$/);
    });
  });

  it.each(MISC)('%s has light/dark parity', (name) => {
    expect(root.has(name)).toBe(true);
    expect(dark.has(name)).toBe(true);
  });

  it('keeps the scrim as the intentional rgba exception in both themes', () => {
    expect(root.get('--overlay-scrim')).toMatch(/^rgba\(.+\)$/);
    expect(dark.get('--overlay-scrim')).toMatch(/^rgba\(.+\)$/);
  });

  it.each(MISC_OKLCH)(
    '%s is an oklch literal in both themes',
    (name) => {
      expect(root.get(name)).toMatch(/^oklch\([^)]*\)$/);
      expect(dark.get(name)).toMatch(/^oklch\([^)]*\)$/);
    },
  );

  it.each(THEMED_ALIASES)('%s is a var() alias that resolves in both themes', (name) => {
    expect(root.get(name)).toMatch(/^var\(--[a-z0-9-]+\)$/);
    expect(dark.get(name)).toMatch(/^var\(--[a-z0-9-]+\)$/);
  });

  describe('cove identity slots', () => {
    it.each(COVE_IDENTITY)('%s is a concrete oklch literal in both themes', (name) => {
      expect(root.get(name)).toMatch(/^oklch\([^)]*\)$/);
      expect(dark.get(name)).toMatch(/^oklch\([^)]*\)$/);
    });

    it('keeps all eight hues pairwise distinct', () => {
      const hues = COVE_IDENTITY.map((name) => {
        const parts = (root.get(name) ?? '').replace(/^oklch\(|\)$/g, '').trim().split(/\s+/);
        return Number(parts[2]);
      });
      expect(hues.every(Number.isFinite)).toBe(true);
      expect(new Set(hues).size).toBe(COVE_IDENTITY.length);
    });
  });

  it.each(SHADOW)('%s is a composite value present in both themes', (name) => {
    expect(root.has(name)).toBe(true);
    expect(dark.has(name)).toBe(true);
  });

  it('locks the dark status design values', () => {
    expect(dark.get('--success')).toBe('oklch(74% 0.14 145)');
    expect(dark.get('--error')).toBe('oklch(74% 0.14 25)');
  });

  it.each(STATUS)('%s is a concrete oklch literal in light mode', (name) => {
    expect(root.get(name)).toMatch(/^oklch\([^)]*\)$/);
  });
});

describe('styles/tokens single-mode scalar contracts', () => {
  it.each(TYPE_SCALE)('%s is px and has no dark override', (name) => {
    expect(root.get(name)).toMatch(/^\d+(?:\.\d+)?px$/);
    expect(dark.has(name)).toBe(false);
  });

  it.each(LEADING)('%s is unitless and has no dark override', (name) => {
    expect(root.get(name)).toMatch(/^\d+(?:\.\d+)?$/);
    expect(dark.has(name)).toBe(false);
  });

  it.each(TRACKING.filter((name) => name !== '--tracking-normal'))(
    '%s is em and has no dark override',
    (name) => {
    expect(root.get(name)).toMatch(/^(?:0|-?0\.\d+em)$/);
    expect(dark.has(name)).toBe(false);
    },
  );

  it('--tracking-normal is the numeric reset', () => {
    expect(root.get('--tracking-normal')).toBe('0');
    expect(dark.has('--tracking-normal')).toBe(false);
  });

  it.each(RADIUS.filter((name) => root.has(name)))('%s is px and has no dark override', (name) => {
    expect(root.get(name)).toMatch(/^\d+(?:\.\d+)?px$/);
    expect(dark.has(name)).toBe(false);
  });

  it.each(SPACING)('%s is zero or px and has no dark override', (name) => {
    expect(root.get(name)).toMatch(/^(?:0|\d+(?:\.\d+)?px)$/);
    expect(dark.has(name)).toBe(false);
  });

  it.each(MOTION)('%s uses seconds and has no dark override', (name) => {
    expect(root.get(name)).toMatch(/^\d+(?:\.\d+)?s$/);
    expect(dark.has(name)).toBe(false);
  });

  it.each(BOX_SCALE)('%s is px and has no dark override', (name) => {
    expect(root.get(name)).toMatch(/^\d+(?:\.\d+)?px$/);
    expect(dark.has(name)).toBe(false);
  });

  it.each(WEIGHTS)('%s is a three-digit weight and has no dark override', (name) => {
    expect(root.get(name)).toMatch(/^[1-9]00$/);
    expect(dark.has(name)).toBe(false);
  });

  it('locks the weight ladder to exactly three steps', () => {
    expect(WEIGHTS.map((name) => Number(root.get(name)))).toEqual([400, 500, 600]);
  });
});

describe('styles/tokens font and stacking contracts', () => {
  it.each(FONT_ALIASES)('%s targets a positional font and has no dark override', (name) => {
    expect(root.get(name)).toMatch(/^var\(--font-(?:sans|serif|mono)\)$/);
    expect(dark.has(name)).toBe(false);
  });

  it('keeps both mono-stack endpoints byte-equal to the independently pinned deployed value', () => {
    const deployed = 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace';
    expect([root.get('--font-mono'), MONO_STACK]).toEqual([deployed, deployed]);
  });

  it.each(Z_INDEX)('%s is a single-mode bare integer', (name) => {
    expect(root.get(name)).toMatch(/^\d+$/);
    expect(dark.has(name)).toBe(false);
  });

  it('keeps all six z-index tiers strictly increasing', () => {
    const values = Z_INDEX.map((name) => Number(root.get(name)));
    expect(values.every(Number.isFinite)).toBe(true);
    expect(values.every((value, index) => index === 0 || values[index - 1] < value)).toBe(true);
  });
});
