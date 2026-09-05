import fs from 'node:fs';

const source = fs.readFileSync(new URL('../../web/src/styles/tokens.css', import.meta.url), 'utf8');
const blocks = { light: source.match(/:root\s*\{([\s\S]*?)\n\s*\}/)?.[1] ?? '', dark: source.match(/\[data-theme="dark"\]\s*\{([\s\S]*?)\n\s*\}/)?.[1] ?? '' };
/** @param {string} text */
const declarations = (text) => new Map([...text.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map((match) => [match[1], match[2].trim()]));
const light = declarations(blocks.light);
const dark = new Map([...light, ...declarations(blocks.dark)]);

/** @param {string} name @param {Map<string, string>} vars */
function resolve(name, vars) {
  let value = vars.get(name);
  for (let i = 0; value?.startsWith('var(') && i < 10; i += 1) value = vars.get(value.slice(4, -1).trim());
  if (!value) throw new Error(`Missing contrast token ${name}`);
  return value;
}
/** @param {string} value @returns {[number, number, number, number]} */
function rgb(value) {
  const match = value.match(/oklch\(([\d.]+)%\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*([\d.]+))?\)/);
  if (!match) throw new Error(`Unsupported colour: ${value}`);
  const L = Number(match[1]) / 100; const C = Number(match[2]); const h = Number(match[3]) * Math.PI / 180;
  const a = C * Math.cos(h); const b = C * Math.sin(h);
  const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
  const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
  const s_ = L - 0.0894841775 * a - 1.291485548 * b;
  const l = l_ ** 3; const m = m_ ** 3; const s = s_ ** 3;
  return [4.0767416621*l - 3.3077115913*m + 0.2309699292*s, -1.2684380046*l + 2.6097574011*m - 0.3413193965*s, -0.0041960863*l - 0.7034186147*m + 1.707614701*s, Number(match[4] ?? 1)];
}
/** @param {[number, number, number, number]} fg @param {readonly number[]} bg */
function composite(fg, bg) { return /** @type {[number, number, number]} */ (fg.slice(0, 3).map((channel, index) => channel * fg[3] + bg[index] * (1 - fg[3]))); }
/** @param {readonly number[]} color */
function luminance(color) { return 0.2126 * color[0] + 0.7152 * color[1] + 0.0722 * color[2]; }
/** @param {readonly number[]} a @param {readonly number[]} b */
function ratio(a, b) { const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x); return (hi + 0.05) / (lo + 0.05); }

/**
 * ── Recipes that live outside `tokens.css` ────────────────────────────────
 *
 * Settings › Plugins paints its five state chips with per-theme `oklch()`
 * declared in its own CSS module rather than with tokens — they are
 * use-specific fills for one column, not new semantic steps, and the module
 * says why at length. That reasoning is only allowed to stand if the fills are
 * still *measured*, so this reads the declarations back out of the stylesheet
 * that owns them: lowering one of those ten values reddens this gate.
 *
 * Fail-closed in both directions. A missing block is an error rather than a
 * skipped recipe, and so is a block that stops declaring a name this expects —
 * the failure mode of the alternative is a rule silently checking nothing.
 *
 * @param {URL} url @param {string} selector
 */
function declarationsInRule(url, selector) {
  const css = fs.readFileSync(url, 'utf8');
  const start = css.indexOf(selector);
  if (start === -1) throw new Error(`Missing rule ${selector} in ${url.pathname}`);
  const open = css.indexOf('{', start);
  const end = css.indexOf('}', open);
  if (open === -1 || end === -1) throw new Error(`Unterminated rule ${selector} in ${url.pathname}`);
  return declarations(css.slice(open, end + 1));
}

const chipModule = new URL('../../web/src/features/settings/settings.module.css', import.meta.url);
const chipLight = declarationsInRule(chipModule, '\n  .pluginStateChip {');
const chipDark = new Map([
  ...chipLight,
  ...declarationsInRule(chipModule, '\n  [data-theme="dark"] .pluginStateChip {'),
]);
/** The chip's fill and the type painted on it, named as astryx names them. */
const chipPairs = [
  { label: 'running chip', foreground: '--color-on-success', background: '--color-success' },
  { label: 'crashed chip', foreground: '--color-on-error', background: '--color-error' },
  { label: 'unavailable chip', foreground: '--color-on-warning', background: '--color-warning' },
  { label: 'spawning/installing/installed chip', foreground: '--color-on-accent', background: '--color-accent' },
  { label: 'unknown chip', foreground: '--color-text-primary', background: '--color-neutral' },
];

let failed = false;

/**
 * ── The list above is checked against the stylesheet, not trusted ─────────
 *
 * `chipPairs` used to be five hand-written rows and nothing tied them to the
 * rule they claim to measure. That made the gate silent on exactly the change
 * it exists for: a sixth `--color-*` override added to `.pluginStateChip` — a
 * new tone for a new state, which is the only reason anyone edits that block —
 * would be painted, shipped and never measured, and this file would still print
 * five green numbers.
 *
 * So the relation is set equality, both ways, over every `--color-*` the rule
 * declares in either theme:
 *
 *   * a declared name the pairs do not mention is an unmeasured colour, and
 *     it is an error rather than a skipped recipe;
 *   * a name the pairs mention that the rule no longer declares is a recipe
 *     measuring an inherited value the chip does not override — the rule went
 *     away and the number kept printing.
 *
 * The `--color-<v>` / `--color-on-<v>` convention is what makes the first half
 * decidable without a browser: astryx's variants paint `--color-on-<v>` on
 * `--color-<v>`, so a declared pair *is* a text-on-fill recipe and owes a
 * ratio. `neutral` is the one that breaks the convention — astryx takes its
 * type from `--color-text-primary` — which is why the check is stated over the
 * declaration set as a whole and not as a rule about name suffixes: an
 * exception has to be listed above to pass, and listing it is the point.
 */
{
  const declared = new Set(
    [...chipLight.keys(), ...chipDark.keys()].filter((name) => name.startsWith('--color-')),
  );
  const measured = new Set(chipPairs.flatMap(({ foreground, background }) => [foreground, background]));
  for (const name of declared) {
    if (!measured.has(name)) {
      failed = true;
      console.error(`.pluginStateChip declares ${name} but no contrast recipe measures it`);
    }
  }
  for (const name of measured) {
    if (!declared.has(name)) {
      failed = true;
      console.error(`contrast recipe names ${name}, which .pluginStateChip no longer declares`);
    }
  }
  /* And the convention itself, so a fill added *with* its `on-` twin cannot be
     covered by naming only one of the two. */
  for (const name of declared) {
    if (name.startsWith('--color-on-')) continue;
    const twin = name.replace('--color-', '--color-on-');
    if (declared.has(twin) && !(measured.has(name) && measured.has(twin))) {
      failed = true;
      console.error(`${name}/${twin} are declared as a pair but are not measured as one`);
    }
  }
}
/** @type {Array<[string, Map<string, string>]>} */
const themes = [['light', light], ['dark', dark]];
/** @type {Array<[string, Map<string, string>]>} */
const chipThemes = [['light', chipLight], ['dark', chipDark]];

for (const [theme, vars] of chipThemes) {
  for (const { label, foreground, background } of chipPairs) {
    const fill = rgb(resolve(background, vars));
    const type = rgb(resolve(foreground, vars));
    /* Both sides, not only the fill. The ratio is computed from this crude
       linear-sRGB conversion, and a channel outside [0, 1] means the value is
       outside the space the number was computed in — a ratio derived from one
       is not a measurement of anything a screen will show, whichever side of
       the pair it came from. The chip owns both names (§6.8 freezes
       `web/src/styles`, and these are scoped overrides, not tokens), so
       checking the type as well costs nothing borrowed. */
    /** @type {ReadonlyArray<[string, readonly number[]]>} */
    const sides = [['fill', fill], ['type', type]];
    for (const [side, color] of sides) {
      if (color.slice(0, 3).some((channel) => channel < 0 || channel > 1)) {
        failed = true;
        console.error(`${theme} ${label} ${side}: outside sRGB gamut`);
      }
    }
    const measured = ratio(type, fill);
    console.log(`${theme} plugin ${label}: ${measured.toFixed(2)}:1`);
    if (measured < 4.5) {
      failed = true;
      console.error(`${theme} plugin ${label}: ${measured.toFixed(2)}:1 (requires 4.50:1)`);
    }
  }
}

/**
 * This is deliberately a small semantic-pair check, not a CSS/DOM contrast
 * audit. It protects only the explicitly listed text/fill recipes below.
 * Actual inherited foregrounds and ancestor backgrounds require a browser
 * render audit and are tracked separately.
 */
const pairs = [
  { label: 'destructive action text on solid error fill', foreground: '--text-on-accent', background: '--error', underlay: '--error' },
  { label: 'warning text on soft warning over card', foreground: '--warn-text', background: '--warn-soft', underlay: '--surface-card' },
  { label: 'warning text on soft warning over paper', foreground: '--warn-text', background: '--warn-soft', underlay: '--paper' },
  { label: 'warning text on page ground', foreground: '--warn-text', background: '--bg', underlay: '--bg' },
  { label: 'warning text on rail', foreground: '--warn-text', background: '--surface-rail', underlay: '--surface-rail' },
  { label: 'warning fill on soft warning over card', foreground: '--warn', background: '--warn-soft', underlay: '--surface-card', minimum: 3 },
  { label: 'warning fill on page ground', foreground: '--warn', background: '--bg', underlay: '--bg', minimum: 3 },
  { label: 'error text on soft error over ground', foreground: '--error-text', background: '--error-soft', underlay: '--bg' },
];
const gamutTokens = ['--warn', '--error-text', ...pairs.flatMap(({ foreground, background }) => [foreground, background])];

for (const [theme, vars] of themes) {
  for (const name of new Set(gamutTokens)) {
    if (rgb(resolve(name, vars)).slice(0, 3).some((channel) => channel < 0 || channel > 1)) {
      failed = true;
      console.error(`${theme} ${name}: outside sRGB gamut`);
    }
  }
  /** @type {ReadonlyArray<{ label: string, foreground: string, background: string, underlay: string, alpha?: number, minimum?: number }>} */
  const recipes = pairs;
  for (const { label, foreground, background, underlay, alpha, minimum = 4.5 } of recipes) {
    const foregroundRgb = rgb(resolve(foreground, vars));
    const fill = rgb(resolve(background, vars));
    const underlayRgb = rgb(resolve(underlay, vars));
    const painted = composite(fill, underlayRgb);
    /* `alpha` is the element's own opacity, which is not a property of the
       token and so is composited here rather than resolved above. Recipes
       without one are left exactly as they were — an opaque foreground
       composites to itself, but doing it unconditionally would silently change
       the number for any token that ships an alpha channel. */
    /** @type {[number, number, number, number]} */
    const dimmed = [foregroundRgb[0], foregroundRgb[1], foregroundRgb[2], foregroundRgb[3] * (alpha ?? 1)];
    const measured = alpha === undefined
      ? ratio(foregroundRgb, painted)
      : ratio(composite(dimmed, painted), painted);
    if (measured < minimum) {
      failed = true;
      console.error(`${theme} ${label}: ${measured.toFixed(2)}:1 (requires ${minimum.toFixed(2)}:1)`);
    }
  }
}
if (failed) process.exitCode = 1;
