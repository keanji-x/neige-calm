import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

function object(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(`${label} must be an object`);
  return value;
}
function keys(value, expected, label) {
  object(value, label);
  if (Object.keys(value).sort().join() !== [...expected].sort().join()) throw new Error(`Unexpected ${label} fields`);
}
function number(value, min, max, label) {
  if (!Number.isFinite(value) || value < min || value > max) throw new Error(`Invalid ${label}`);
  return value;
}
function text(value, label) {
  if (typeof value !== 'string' || !value.trim() || /[;{}\n\r]/.test(value)) throw new Error(`Invalid ${label}`);
  return value;
}
function declarations(values, label) {
  return Object.entries(object(values, label)).map(([name, value]) => {
    if (!/^--[a-z][a-z0-9-]*$/.test(name)) throw new Error(`Invalid token ${name}`);
    return `    ${name}: ${text(value, name)};`;
  });
}

/** Build-time owner: no runtime imports, selector scanning or state overrides. */
export function renderTheme(input) {
  const config = object(input, 'theme');
  if (config.version !== 1) throw new Error('Unsupported theme version');
  keys(config, ['version', 'typography', 'navigation', 'shared', 'light', 'dark', 'hostRgb', 'terminal', 'terminalText'], 'theme');
  const type = object(config.typography, 'typography');
  keys(type, ['families', 'sizes', 'weights', 'lines', 'tracking', 'roles'], 'typography');
  keys(type.families, ['ui', 'reading', 'code'], 'families');
  keys(type.tracking, ['normal', 'label'], 'tracking');
  keys(type.sizes, ['caption', 'ui', 'reading', 'heading', 'metric'], 'sizes');
  keys(type.weights, ['regular', 'medium', 'semibold', 'bold'], 'weights');
  keys(type.lines, ['caption', 'ui', 'reading', 'heading', 'metric'], 'lines');
  const rem = value => `${value / 16}rem`;
  for (const [name, value] of Object.entries(object(type.sizes, 'sizes'))) number(value, 10, 40, name);
  for (const [name, value] of Object.entries(object(type.lines, 'lines'))) number(value, 12, 64, name);
  for (const [name, value] of Object.entries(object(type.weights, 'weights'))) number(value, 100, 900, name);
  for (const [name, value] of Object.entries(object(type.tracking, 'tracking'))) number(value, -2, 4, name);
  keys(type.roles, ['ui', 'action', 'detail', 'page-title', 'navigation-label', 'reading', 'chapter', 'table', 'label', 'metadata', 'code', 'content', 'metric', 'metric-primary', 'heading', 'subheading', 'subheading-small'], 'roles');
  const familyTokens = { ui: '--font-sans', reading: '--font-serif', code: '--font-mono' };
  const generated = {};
  for (const [name, token] of Object.entries(familyTokens)) generated[token] = text(type.families?.[name], name);
  for (const [name, key] of Object.entries({ xs: 'caption', meta: 'caption', base: 'ui', md: 'reading', lg: 'reading', xl: 'heading' })) {
    generated[`--text-${name}`] = rem(number(type.sizes[key], 10, 40, key));
  }
  for (const [name, key] of Object.entries({ normal: 'regular', medium: 'medium', semibold: 'semibold', bold: 'bold' })) {
    generated[`--weight-${name}`] = String(number(type.weights[key], 100, 900, key));
  }
  generated['--tracking-normal'] = type.tracking.normal === 0 ? '0' : rem(type.tracking.normal);
  generated['--tracking-label'] = rem(type.tracking.label);
  for (const name of ['tighter', 'tight', 'wide']) generated[`--tracking-${name}`] = 'var(--tracking-normal)';
  for (const name of ['wider', 'widest']) generated[`--tracking-${name}`] = 'var(--tracking-label)';
  for (const [name, value] of Object.entries(type.lines)) generated[`--line-${name}`] = rem(value);
  for (const [name, raw] of Object.entries(object(type.roles, 'roles'))) {
    if (!/^[a-z]+(?:-[a-z]+)*$/.test(name)) throw new Error(`Invalid role ${name}`);
    const role = object(raw, name);
    keys(role, ['family', 'size', 'weight', 'line'], name);
    for (const [field, group] of Object.entries({ size: 'sizes', line: 'lines', weight: 'weights' })) {
      if (!Object.hasOwn(type[group], role[field])) throw new Error(`${name}.${field} references an unknown token`);
    }
    if (!Object.hasOwn(familyTokens, role.family)) throw new Error(`Invalid ${name} font family`);
    if (type.lines[role.line] < type.sizes[role.size]) throw new Error(`${name} line height is smaller than font size`);
    generated[`--type-${name}`] = `${type.weights[role.weight]} ${rem(type.sizes[role.size])}/var(--line-${role.line}) var(${familyTokens[role.family]})`;
  }
  const nav = object(config.navigation, 'navigation');
  const navLimits = { railWidth: [200, 400], rowHeight: [24, 48], headerHeight: [32, 56], moduleRowHeight: [20, 40], groupGap: [0, 24], sectionGap: [0, 32], labelInset: [0, 12], railMix: [0, 100] };
  keys(nav, Object.keys(navLimits), 'navigation');
  for (const [name, value] of Object.entries(nav)) number(value, navLimits[name][0], navLimits[name][1], name);
  if (nav.rowHeight < type.lines.ui) throw new Error('Navigation row clips its text line');
  for (const [name, value] of Object.entries(nav)) {
    if (name === 'railMix') continue;
    generated[`--navigation-${name.replace(/[A-Z]/g, letter => `-${letter.toLowerCase()}`)}`] = rem(value);
  }
  generated['--surface-navigation'] = `color-mix(in srgb, var(--surface-rail) ${nav.railMix}%, var(--bg))`;
  const shared = object(config.shared, 'shared');
  for (const name of Object.keys(generated)) if (Object.hasOwn(shared, name)) throw new Error(`Duplicate token ${name}`);
  const light = object(config.light, 'light'), dark = object(config.dark, 'dark');
  if (Object.keys(light).sort().join() !== Object.keys(dark).sort().join()) throw new Error('Light/dark token inventory differs');
  for (const name of Object.keys(light)) if (Object.hasOwn(shared, name) || Object.hasOwn(generated, name)) throw new Error(`Themed token overlaps shared token ${name}`);
  return `/* Generated from theme.config.json. Run npm run theme:generate; do not edit. */\n@layer tokens {\n  :root {\n${declarations({ ...shared, ...generated, ...light }, 'root').join('\n')}\n  }\n\n  [data-theme="dark"] {\n${declarations(dark, 'dark').join('\n')}\n  }\n}\n`;
}

/** Static host values live in the styles leaf; no runtime theme resolution. */
export function renderThemeValues(input) {
  renderTheme(input);
  const host = object(input.hostRgb, 'hostRgb'), terminal = object(input.terminal, 'terminal');
  keys(host, ['light', 'dark'], 'hostRgb');keys(terminal, ['light', 'dark'], 'terminal');
  keys(input.terminalText, ['fontSize'], 'terminalText');
  const terminalSize = number(input.terminalText.fontSize, 10, 24, 'terminalText.fontSize');
  const exports = [`export const TERMINAL_FONT_SIZE = ${terminalSize};`, `export const MONO_STACK = ${JSON.stringify(input.typography.families.code)};`];
  for (const mode of ['light', 'dark']) {
    const rgb = object(host[mode], `hostRgb.${mode}`);keys(rgb, ['fg', 'bg'], `hostRgb.${mode}`);
    for (const side of ['fg', 'bg']) {
      if (!Array.isArray(rgb[side]) || rgb[side].length !== 3) throw new Error(`Invalid ${mode}.${side} tuple`);
      for (const byte of rgb[side]) if (!Number.isInteger(number(byte, 0, 255, `${mode}.${side}`))) throw new Error('RGB must be integer bytes');
    }
    exports.push(`export const ${mode.toUpperCase()}_THEME_RGB = Object.freeze({\n  fg: Object.freeze(${JSON.stringify(rgb.fg)} as const),\n  bg: Object.freeze(${JSON.stringify(rgb.bg)} as const),\n});`);
    const palette = object(terminal[mode], `terminal.${mode}`);
    keys(palette, ['background', 'foreground', 'cursor', 'cursorAccent', 'selectionBackground', 'black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white', 'brightBlack', 'brightRed', 'brightGreen', 'brightYellow', 'brightBlue', 'brightMagenta', 'brightCyan', 'brightWhite'], `terminal.${mode}`);
    for (const [name, value] of Object.entries(palette)) {
      if (!/^[a-zA-Z]+$/.test(name)) throw new Error(`Invalid terminal color ${name}`);
      text(value, `terminal.${mode}.${name}`);
      if (!/^(?:#[\da-fA-F]{6}(?:[\da-fA-F]{2})?|rgba?\([\d.,% ]+\))$/.test(value)) throw new Error(`Invalid terminal color ${name}`);
    }
    exports.push(`export const ${mode.toUpperCase()}_TERMINAL_THEME = Object.freeze(${JSON.stringify(palette, null, 2)});`);
  }
  return `/* Generated from theme.config.json. Run npm run theme:generate; do not edit. */\n${exports.join('\n\n')}\n`;
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(import.meta.filename)) {
  const root = new URL('../../web/src/styles/', import.meta.url);
  const config = JSON.parse(readFileSync(new URL('theme.config.json', root), 'utf8'));
  const outputs = { 'tokens.css': renderTheme(config), 'theme-values.ts': renderThemeValues(config) };
  for (const [name, content] of Object.entries(outputs)) {
    const output = new URL(name, root);
    if (process.argv.includes('--check')) {
      if (readFileSync(output, 'utf8') !== content) throw new Error(`${name} is stale; run npm run theme:generate`);
    } else writeFileSync(output, content);
  }
  if (process.argv.includes('--check')) console.log('Central theme configuration and generated CSS/host values agree');
}
