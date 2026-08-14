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
/** @param {[number, number, number, number]} fg @param {[number, number, number, number]} bg */
function composite(fg, bg) { return /** @type {[number, number, number]} */ (fg.slice(0, 3).map((channel, index) => channel * fg[3] + bg[index] * (1 - fg[3]))); }
/** @param {readonly number[]} color */
function luminance(color) { return 0.2126 * color[0] + 0.7152 * color[1] + 0.0722 * color[2]; }
/** @param {readonly number[]} a @param {readonly number[]} b */
function ratio(a, b) { const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x); return (hi + 0.05) / (lo + 0.05); }

let failed = false;
/** @type {Array<[string, Map<string, string>]>} */
const themes = [['light', light], ['dark', dark]];

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
  for (const { label, foreground, background, underlay, minimum = 4.5 } of pairs) {
    const foregroundRgb = rgb(resolve(foreground, vars));
    const fill = rgb(resolve(background, vars));
    const underlayRgb = rgb(resolve(underlay, vars));
    const measured = ratio(foregroundRgb, composite(fill, underlayRgb));
    if (measured < minimum) {
      failed = true;
      console.error(`${theme} ${label}: ${measured.toFixed(2)}:1 (requires ${minimum.toFixed(2)}:1)`);
    }
  }
}
if (failed) process.exitCode = 1;
