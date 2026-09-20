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
 * Reads a recipe's declarations back out of the stylesheet that owns it. Fail-closed: a missing
 * block is an error, not a skipped recipe.
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

/*
 * `chipPairs` is checked against the stylesheet by set equality over every `--color-*` the rule
 * declares in either theme: a declared name no pair measures is an error, and so is a measured name
 * the rule no longer declares. `neutral` breaks the `--color-<v>` / `--color-on-<v>` convention, which
 * is why the check is over the declaration set and not name suffixes.
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
    /* Both sides: a channel outside [0, 1] means the value is outside the space the ratio was computed
           in, whichever side of the pair it came from. */
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

/** A small semantic-pair check over the listed text/fill recipes, not a CSS/DOM contrast audit. */
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
    /* `alpha` is the element's own opacity, not a property of the token, so it is composited here;
           recipes without one are left exactly as they were. */
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
