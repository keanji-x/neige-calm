/* The full surface x foreground contrast matrix, measured in a real browser: browsers gamut-map out-of-sRGB oklch differently, so an arithmetic table can pass while the shipped pixel fails. */
import { beforeAll, describe, expect, it } from 'vitest';
import './tokens.css';
import tokensSource from './tokens.css?raw';

type Rgb = [number, number, number];
type Rgba = [number, number, number, number];

/* The surface census is read out of `tokens.css` itself, then asserted equal to a written-down census so adding a surface is a reviewed decision. */
/* Custom property names are case-sensitive idents, so the class is wider than `[a-z0-9-]`; comments are stripped first. Must match `tokens.contract.test.ts`'s definition. */
const DECLARATION = /(--[A-Za-z0-9_\u00A0-\uFFFF-]+)\s*:/g;
const DECLARED = [
  ...new Set(
    [...tokensSource.replace(/\/\*[\s\S]*?\*\//g, '').matchAll(DECLARATION)].map(
      (match) => match[1] ?? '',
    ),
  ),
];
const DERIVED_SURFACES = DECLARED.filter(
  (name) => name === '--bg' || name === '--paper' || name.startsWith('--surface-'),
).sort();

/** Surfaces that paint a pixel of their own. Verified against the measurement below. */
const OPAQUE_CENSUS = [
  '--bg', '--paper', '--surface-card', '--surface-chip', '--surface-rail', '--surface-terminal',
] as const;
/** Surfaces that are an alpha tint over whatever they land on: measured for being alpha and excluded from the matrix. */
const ALPHA_CENSUS = ['--surface-code'] as const;

const SURFACES: readonly string[] = DERIVED_SURFACES.filter(
  (name) => !(ALPHA_CENSUS as readonly string[]).includes(name),
);

/** The +2 rank and the terminal, which spend no lightness over `--surface-card`. */
const PLUS_TWO = ['--paper', '--surface-terminal'] as const;
const PINNED_TO_CARD: readonly string[] = ['--surface-card', ...PLUS_TWO];

/** Foregrounds that carry text: WCAG 2.x AA body floor. */
const TEXT = ['--text', '--text-2', '--text-3', '--warn-text', '--error-text', '--success-text'] as const;
/** Foregrounds that frame a control or carry state: non-text floor. */
const NON_TEXT = ['--accent', '--warn', '--error', '--success'] as const;
/** Purely decorative; exempt, but must not silently collapse onto its surface. */
const DECORATIVE = ['--hairline', '--hairline-strong', '--text-4'] as const;

const AA_TEXT = 4.5;
const AA_NON_TEXT = 3;
/** A decorative line still has to be a line. */
const VISIBLE = 1.15;

let probe: HTMLElement;
let ctx: CanvasRenderingContext2D;

/* Chromium returns `getComputedStyle().color` in the authored colour space, so the string says nothing about the pixel; canvas runs the same parse and gamut mapping as the compositor. */
function paint(cssColor: string): Rgba {
  /* Clear first: the 1x1 canvas is reused and `fillRect` composites source-over, so a token with alpha would blend into the previous paint. A misspelled token falls back to black in the cascade, not here. */
  ctx.clearRect(0, 0, 1, 1);
  ctx.fillStyle = cssColor;
  ctx.fillRect(0, 0, 1, 1);
  const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
  return [r, g, b, a];
}

/** The cascade's answer for a custom property, in whatever space it was authored. */
function computed(token: string, theme: 'light' | 'dark'): string {
  if (theme === 'dark') document.documentElement.dataset.theme = 'dark';
  else delete document.documentElement.dataset.theme;
  probe.style.color = `var(${token})`;
  return getComputedStyle(probe).color;
}

/** Resolve a custom property to the pixel the browser would paint. */
function resolve(token: string, theme: 'light' | 'dark'): Rgb {
  const [r, g, b] = paint(computed(token, theme));
  return [r, g, b];
}

/** 255 for an opaque token; below that it is a tint over its backdrop. */
function opacity(token: string, theme: 'light' | 'dark'): number {
  return paint(computed(token, theme))[3];
}

const channel = (v: number) => {
  const c = v / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};
const luminance = ([r, g, b]: Rgb) => 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);

/** sRGB bytes -> OKLCH: the ladder is specified in L and C, so those assertions are made in those units. */
function srgbToOklch(R: number, G: number, B: number): [number, number, number] {
  const [r, g, b] = [R, G, B].map((v) => channel(v));
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const L = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
  const A = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const Bb = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  return [L, Math.hypot(A, Bb), ((Math.atan2(Bb, A) * 180) / Math.PI + 360) % 360];
}
function contrast(a: Rgb, b: Rgb) {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

beforeAll(() => {
  probe = document.createElement('div');
  document.body.append(probe);
  const canvas = document.createElement('canvas');
  canvas.width = 1;
  canvas.height = 1;
  const got = canvas.getContext('2d', { willReadFrequently: true });
  if (!got) throw new Error('no 2d context');
  ctx = got;
});

describe('the surface census this matrix runs over', () => {
  /* The meta-test: a new `--surface-*` line in `tokens.css` fails here until it is written down. */
  it('is exactly what tokens.css declares', () => {
    expect(DERIVED_SURFACES).toEqual([...OPAQUE_CENSUS, ...ALPHA_CENSUS].sort());
  });

  /* A typo in a hand-written foreground list does not fail on its own: `var(--text-33)` is invalid at computed-value time, `color` inherits, and the probe measures the body's black. */
  it('every foreground it measures is a token tokens.css declares', () => {
    const undeclared = [...TEXT, ...NON_TEXT, ...DECORATIVE].filter(
      (token) => !DECLARED.includes(token),
    );
    expect(undeclared).toEqual([]);
  });

  /* Which surfaces are tints is read off the painted pixel, not off the census. */
  it.each(['light', 'dark'] as const)('%s: the alpha members are the measured ones', (theme) => {
    const translucent = DERIVED_SURFACES.filter((token) => opacity(token, theme) < 255);
    expect(translucent).toEqual([...ALPHA_CENSUS].sort());
  });
});

describe.each(['light', 'dark'] as const)('%s theme contrast matrix', (theme) => {
  it.each(TEXT.flatMap((fg) => SURFACES.map((bg) => [fg, bg] as const)))(
    '%s on %s clears the AA body floor',
    (fg, bg) => {
      expect(contrast(resolve(fg, theme), resolve(bg, theme))).toBeGreaterThanOrEqual(AA_TEXT);
    },
  );

  it.each(NON_TEXT.flatMap((fg) => SURFACES.map((bg) => [fg, bg] as const)))(
    '%s on %s clears the non-text floor',
    (fg, bg) => {
      expect(contrast(resolve(fg, theme), resolve(bg, theme))).toBeGreaterThanOrEqual(AA_NON_TEXT);
    },
  );

  it.each(DECORATIVE.flatMap((fg) => SURFACES.map((bg) => [fg, bg] as const)))(
    '%s stays visible against %s',
    (fg, bg) => {
      expect(contrast(resolve(fg, theme), resolve(bg, theme))).toBeGreaterThanOrEqual(VISIBLE);
    },
  );

  /* A contrast floor only ever looks at a foreground against a background, never at two backgrounds a component might nest. */
  it('no two surfaces render the same pixel', () => {
    const seen = new Map<string, string>();
    const collisions: string[] = [];
    for (const token of SURFACES) {
      const key = resolve(token, theme).join(',');
      const prior = seen.get(key);
      if (prior && !(PINNED_TO_CARD.includes(prior) && PINNED_TO_CARD.includes(token))) {
        collisions.push(`${token} === ${prior}`);
      }
      if (!prior) seen.set(key, token);
    }
    expect(collisions).toEqual([]);
  });

  /* +2 spends no lightness: equality, not a floor, because the +2 rank is a shadow and a step spent here is taken from the text on the rank below. */
  it.each(PLUS_TWO)('%s is the same pixel as --surface-card (+2 spends no lightness)', (token) => {
    expect(resolve(token, theme)).toEqual(resolve('--surface-card', theme));
  });

  const LADDER = ['--surface-rail', '--surface-chip', '--bg', '--surface-card'] as const;

  it('the ladder runs the same direction in both themes', () => {
    const ls = LADDER.map((t) => luminance(resolve(t, theme)));
    expect(ls.every((v, i) => i === 0 || v > ls[i - 1])).toBe(true);
  });

  /* A surface step may stand as a boundary on its own only above 3.0 L; this decides whether `panel-card`'s "no outline" is still legal. */
  it.each(LADDER.slice(1).map((t, i) => [LADDER[i], t] as const))(
    '%s → %s is at least 3.0 L apart',
    (lower, upper) => {
      const [a] = srgbToOklch(...resolve(lower, theme));
      const [b] = srgbToOklch(...resolve(upper, theme));
      expect((b - a) * 100).toBeGreaterThanOrEqual(3.0);
    },
  );

  /* The chroma gap between the ground and the card: across established light palettes it is never above 0.005, and 0.022 reads as a jarring white card. */
  it('ground and card stay in one hue family (|ΔC| ≤ 0.005)', () => {
    const [, groundC] = srgbToOklch(...resolve('--bg', theme));
    const [, cardC] = srgbToOklch(...resolve('--surface-card', theme));
    expect(Math.abs(cardC - groundC)).toBeLessThanOrEqual(0.005);
  });
});
