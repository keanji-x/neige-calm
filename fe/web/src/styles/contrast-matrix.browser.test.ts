/*
 * The full surface x foreground contrast matrix, measured in a real browser.
 *
 * fe-design.md §10-7 asks for exactly this and says why an arithmetic table
 * does not substitute: browsers gamut-map out-of-sRGB oklch differently, so a
 * spreadsheet can pass while the shipped pixel fails. `check-contrast.mjs` is
 * deliberately a *point* check over eight named recipes and parses the CSS
 * text; this file resolves the tokens through the cascade and reads back what
 * the compositor actually produced.
 *
 * It is a matrix, not a list, because the failures that survive review are the
 * pairs nobody thought to name — the shipped tree had `--surface-chip` equal to
 * `--surface-rail` in both themes, which no point check covered.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import './tokens.css';
import tokensSource from './tokens.css?raw';

type Rgb = [number, number, number];
type Rgba = [number, number, number, number];

/*
 * ── The surface census is derived, not transcribed ──────────────────────────
 *
 * A hand-copied list here is the same funnel this file was written to close.
 * `tokens.contract.test.ts` pins the token inventory by set equality, so a new
 * surface must be declared there — but declaring it there used to be enough to
 * stay green, and this matrix would simply never measure it. The list below is
 * therefore read out of `tokens.css` itself.
 *
 * The naming convention is what carries the meaning: the CSS says nothing about
 * what a token is *for*, so a surface is `--bg`, `--paper`, or `--surface-*`. A
 * surface named outside that convention is the one thing this derivation cannot
 * see, which is why the derived set is also asserted equal to a written-down
 * census (`OPAQUE_CENSUS` / `ALPHA_CENSUS`): adding a surface is then a decision
 * a reviewer signs off on, not a name a test quietly failed to notice.
 */
/*
 * The character class is not `[a-z0-9-]`. CSS custom property names are
 * case-sensitive idents, so `--surfaceGhost` and `--surface_ghost` are both
 * legal and both were invisible to the narrower class — a token declared under
 * either spelling was seen by neither this derivation nor the inventory
 * contract in `tokens.contract.test.ts`, which shared the same class. Comments
 * are stripped first so that a commented-out line is not a declaration; that is
 * the same definition `tokens.contract.test.ts` uses, and the two files must
 * agree on it or one of them reddens on a declaration the other cannot see.
 */
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
/** Surfaces that are an alpha tint over whatever they land on. A contrast floor
 *  against one of these is meaningless without naming the backdrop too, so they
 *  are measured for *being* alpha and excluded from the matrix itself. */
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

/*
 * Chromium now returns `getComputedStyle().color` in the authored colour space
 * ("oklch(0.175 0.007 66)"), so reading the string back tells us nothing about
 * the pixel. Painting it does: canvas runs the same parse and the same
 * out-of-gamut mapping as the compositor, and `getImageData` hands back the
 * sRGB bytes that actually reach the screen. That is the measurement §10-7
 * asks for and the one an arithmetic table cannot make.
 */
function paint(cssColor: string): Rgba {
  /*
   * Clear first. The 1x1 canvas is reused by every measurement in this file and
   * `fillRect` composites source-over, so a token with alpha would blend into
   * whatever the *previous* test painted and the suite's answers would depend
   * on the order it happened to run in. Everything in `SURFACES` and `TEXT` is
   * opaque today, so this changes no current result — it is what makes the
   * alpha classification below a measurement, and what keeps the first
   * translucent token added to any list from being a heisenbug.
   *
   * There is no unparseable-value branch to guard here, and it matters that the
   * comment does not claim one: the only caller feeds this the return of
   * `getComputedStyle().color`, which a browser always serializes as a valid
   * colour, so a `#000` prime beneath would be dead code. A misspelled token
   * *does* fall back to black — but in the cascade, not here: `var(--nope)` is
   * invalid at computed-value time and `color` is inherited, so the probe
   * silently takes the body's colour and `paint` is handed a perfectly valid
   * `rgb(0, 0, 0)`. Nothing in this function can tell that apart from a token
   * that is genuinely black, which is why the census below asserts every
   * foreground name is declared instead.
   */
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

/** sRGB bytes -> OKLCH. The ladder is specified in L and C, so the assertions
 *  about it have to be made in those units, not in WCAG ratios. */
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
  /*
   * The meta-test. Without it, `DERIVED_SURFACES` still measures a new token
   * automatically — but nothing forces anyone to *look* at it, and a surface
   * that happens to clear every floor would be adopted in silence. With it, a
   * new `--surface-*` line in `tokens.css` fails here until it is written down,
   * next to the two words that say which kind it is.
   */
  it('is exactly what tokens.css declares', () => {
    expect(DERIVED_SURFACES).toEqual([...OPAQUE_CENSUS, ...ALPHA_CENSUS].sort());
  });

  /*
   * The same guarantee for the *foreground* side, which had none. Backgrounds
   * are derived from `tokens.css`, so a name that is not declared cannot enter
   * `SURFACES` at all; the foreground lists are hand-written, and a typo in one
   * of them does not fail — `var(--text-33)` is invalid at computed-value time,
   * `color` inherits, and the probe measures the body's black. Against a dark
   * surface that black is a *worse* contrast than the real token, so the dark
   * half reddens and reads like a palette regression; against a light surface
   * it clears every floor and the cell passes having measured nothing. A
   * misspelt `--hairline-strong` left eleven of twelve cells green.
   */
  it('every foreground it measures is a token tokens.css declares', () => {
    const undeclared = [...TEXT, ...NON_TEXT, ...DECORATIVE].filter(
      (token) => !DECLARED.includes(token),
    );
    expect(undeclared).toEqual([]);
  });

  /* Which surfaces are tints is read off the painted pixel, not off the census:
     an author who mis-files an alpha token as opaque gets a red here rather than
     a contrast number computed against a colour nothing ever renders. */
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

  /*
   * Two surfaces rendering the same pixel is how `.avatar` went invisible in
   * the rail: nothing failed, because a contrast floor only ever looks at a
   * foreground *against* a background, never at two backgrounds that a
   * component might place inside one another.
   */
  it('no two surfaces render the same pixel', () => {
    const seen = new Map<string, string>();
    const collisions: string[] = [];
    for (const token of SURFACES) {
      const key = resolve(token, theme).join(',');
      const prior = seen.get(key);
      // The +2 rank is one value on purpose; the assertion below is what makes
      // that a claim this suite enforces rather than an excuse it accepts here.
      if (prior && !(PINNED_TO_CARD.includes(prior) && PINNED_TO_CARD.includes(token))) {
        collisions.push(`${token} === ${prior}`);
      }
      if (!prior) seen.set(key, token);
    }
    expect(collisions).toEqual([]);
  });

  /*
   * **+2 spends no lightness.** This is the one assertion behind three separate
   * claims: `tokens.css`'s note that `--paper` equals `--surface-card` and is
   * separated by `--shadow-float` alone; fe-design.md §6.5's "`--paper`↔`--bg`
   * is 3.4 in light"; and `panel-card`'s licence to draw no border. Until this
   * existed all three rested on a value no gate read — `--paper` could be moved
   * to 1 L off the ground and the whole suite stayed green, because the two
   * tokens are backgrounds and every other check compares a foreground to one
   * background.
   *
   * Equality, not a floor, because equality is what the ladder actually says:
   * the +2 rank is a shadow, and a lightness step spent here is a step taken
   * away from the text standing on the rank below. It also carries the rungs —
   * `--surface-card`'s 3.0 L rung below *is* `--paper`'s rung, given this.
   */
  it.each(PLUS_TWO)('%s is the same pixel as --surface-card (+2 spends no lightness)', (token) => {
    expect(resolve(token, theme)).toEqual(resolve('--surface-card', theme));
  });

  /* Not a rung each: see the pinning assertion above for why `--paper` and
     `--surface-terminal` ride on `--surface-card`'s rung instead. */
  const LADDER = ['--surface-rail', '--surface-chip', '--bg', '--surface-card'] as const;

  it('the ladder runs the same direction in both themes', () => {
    const ls = LADDER.map((t) => luminance(resolve(t, theme)));
    expect(ls.every((v, i) => i === 0 || v > ls[i - 1])).toBe(true);
  });

  /*
   * §6.5: a surface step may stand as a boundary on its own only above 3.0 L.
   * Every rung here is meant to clear that, and light's card rung clears it by
   * 0.4 — so this is the assertion that decides whether `panel-card`'s
   * "no outline" is still legal. Compress the light ladder and this fails
   * before anything looks wrong.
   */
  it.each(LADDER.slice(1).map((t, i) => [LADDER[i], t] as const))(
    '%s → %s is at least 3.0 L apart',
    (lower, upper) => {
      const [a] = srgbToOklch(...resolve(lower, theme));
      const [b] = srgbToOklch(...resolve(upper, theme));
      expect((b - a) * 100).toBeGreaterThanOrEqual(3.0);
    },
  );

  /*
   * The chroma gap between the ground and the surface raised off it. Measured
   * across eight established light palettes it is never above 0.005; the pass
   * that ran 0.022 is what read as a jarring white card. Nothing else in the
   * suite can see this: it is a relationship between two *backgrounds*, and
   * every contrast floor only ever compares a foreground to one background.
   */
  it('ground and card stay in one hue family (|ΔC| ≤ 0.005)', () => {
    const [, groundC] = srgbToOklch(...resolve('--bg', theme));
    const [, cardC] = srgbToOklch(...resolve('--surface-card', theme));
    expect(Math.abs(cardC - groundC)).toBeLessThanOrEqual(0.005);
  });
});
