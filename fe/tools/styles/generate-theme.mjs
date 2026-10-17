import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { typographySchema } from './theme-schema.mjs';

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
function rem(value) { return `${value / 16}rem`; }

function roleRecipe(type, name) { return type.roles[name]; }
function roleStep(type, name) { return { ...type.scale, ...type.exceptions }[roleRecipe(type, name).step]; }

/** Every emitted token keeps its semantic category and inherited-family dependency. */
function tokenRegistry() {
  const records = new Map();
  /** @param {string} name @param {string} value @param {string | null} category */
  const add = (name, value, category = null, fontDependency = false) => {
    if (records.has(name)) throw new Error(`Duplicate token ${name}`);
    if (!/^--[a-z][a-z0-9-]*$/.test(name)) throw new Error(`Invalid token ${name}`);
    records.set(name, { name, value: text(value, name), category, fontDependency });
  };
  return { add, values: () => [...records.values()] };
}

function typographyRecords(type) {
  const familyTokens = { ui: '--font-sans', reading: '--font-serif', code: '--font-mono' };
  const registry = tokenRegistry();
  for (const [name, token] of Object.entries(familyTokens)) registry.add(token, type.families[name]);
  for (const [name, key] of Object.entries({ normal: 'regular', medium: 'medium', semibold: 'semibold', bold: 'bold' })) {
    registry.add(`--weight-${name}`, String(type.weights[key]));
  }
  for (const name of Object.keys(type.roles)) {
    const recipe = roleRecipe(type, name), step = roleStep(type, name);
    const family = `var(${familyTokens[recipe.family]})`;
    registry.add(`--type-${name}`, `${type.weights[recipe.weight]} ${rem(step.size)}/${rem(step.line)} ${family}`, 'TypographyToken', true);
    registry.add(`--type-${name}-tracking`, `${step.tracking}em`, 'TrackingToken');
    registry.add(`--type-${name}-family`, family, 'FontProjectionToken', true);
    registry.add(`--type-${name}-size`, rem(step.size), 'TypeScaleToken');
    registry.add(`--type-${name}-line`, rem(step.line), 'LeadingToken');
    registry.add(`--type-${name}-leading`, String(step.line / step.size), 'LeadingToken');
  }
  for (const [name, step] of Object.entries(type.scale)) {
    registry.add(`--font-step-${name}-size`, rem(step.size), 'TypeScaleToken');
    registry.add(`--font-step-${name}-line`, rem(step.line), 'LeadingToken');
    registry.add(`--font-step-${name}-tracking`, `${step.tracking}em`, 'TrackingToken');
  }
  return registry.values();
}

function themeRecords(input) {
  const config = object(input, 'theme');
  if (config.version !== 2) throw new Error('Unsupported theme version');
  keys(config, ['version', 'typography', 'navigation', 'shared', 'light', 'dark', 'hostRgb', 'terminal', 'terminalText', 'spacing'], 'theme');
  const type = typographySchema.parse(config.typography);
  const registry = tokenRegistry();
  for (const [name, value] of Object.entries(object(config.shared, 'shared'))) {
    registry.add(name, value, name === '--leading-none' ? 'LeadingToken' : null,
      ['--font-display', '--font-numeric', '--font-code'].includes(name));
  }
  for (const record of typographyRecords(type)) registry.add(record.name, record.value, record.category, record.fontDependency);
  keys(config.spacing, ['0', 'px', 'half', '1', '2', '3', '4', '5', '6', '7', '8', '9'], 'spacing');
  for (const [name, value] of Object.entries(config.spacing)) {
    registry.add(`--space-${name}`, value === 0 ? '0' : `${number(value, 0, 64, `spacing.${name}`)}px`, 'SpacingToken');
  }
  const nav = object(config.navigation, 'navigation');
  const navLimits = { railWidth: [200, 400], rowHeight: [24, 48], headerHeight: [32, 56], moduleRowHeight: [20, 40], railMix: [0, 100] };
  keys(nav, [...Object.keys(navLimits), 'groupGap', 'sectionGap', 'labelInset'], 'navigation');
  for (const name of Object.keys(navLimits)) number(nav[name], navLimits[name][0], navLimits[name][1], name);
  for (const name of ['groupGap', 'sectionGap', 'labelInset']) if (!Object.hasOwn(config.spacing, nav[name])) throw new Error(`navigation.${name} references an unknown spacing step`);
  if (nav.rowHeight < roleStep(type, 'ui').line) throw new Error('Navigation row clips its text line');
  for (const [name, value] of Object.entries(nav)) {
    if (name === 'railMix') continue;
    registry.add(`--navigation-${name.replace(/[A-Z]/g, letter => `-${letter.toLowerCase()}`)}`,
      Object.hasOwn(navLimits, name) ? rem(value) : `var(--space-${value})`, 'NavigationToken');
  }
  registry.add('--surface-navigation', `color-mix(in srgb, var(--surface-rail) ${nav.railMix}%, var(--bg))`);
  const light = object(config.light, 'light'), dark = object(config.dark, 'dark');
  if (Object.keys(light).sort().join() !== Object.keys(dark).sort().join()) throw new Error('Light/dark token inventory differs');
  for (const [name, value] of Object.entries(light)) registry.add(name, value);
  const darkRegistry = tokenRegistry();
  for (const [name, value] of Object.entries(dark)) darkRegistry.add(name, value);
  return { root: registry.values(), dark: darkRegistry.values() };
}

function recordDeclarations(records) { return records.map(({ name, value }) => `    ${name}: ${text(value, name)};`); }

/** Build-time owner: validate the complete registry before any output is written. */
export function renderTheme(input) {
  const records = themeRecords(input);
  return `/* Generated from theme.config.json. Run npm run theme:generate; do not edit. */\n@layer tokens {\n  :root {\n${recordDeclarations(records.root).join('\n')}\n  }\n\n  [data-theme="dark"] {\n${recordDeclarations(records.dark).join('\n')}\n  }\n}\n`;
}

/** Recompute exactly the emissions that depend on an inherited font family. */
export function renderTypographyScope(input) {
  const records = themeRecords(input).root.filter(record => record.fontDependency);
  return `/* Generated from theme.config.json. Run npm run theme:generate; do not edit. */\n@layer ui {\n  .scope {\n${recordDeclarations(records).join('\n')}\n  }\n}\n`;
}

/** Closed public types and inventory consume emission metadata, never token-name suffixes. */
export function renderTypographyTypes(input) {
  const groups = new Map();
  const records = themeRecords(input).root.filter(record => record.category !== null);
  for (const { category, name } of records) {
    if (!groups.has(category)) groups.set(category, []);
    groups.get(category).push(name);
  }
  const inventory = records.map(record => record.name).sort();
  const categories = [...groups].map(([name, values]) => `  ${name}: Object.freeze(${JSON.stringify(values)} as const),`);
  const declarations = [...groups.keys()].map(name => `export type ${name} = (typeof GENERATED_TOKEN_CATEGORIES.${name})[number];`);
  return `/* Generated from theme.config.json. Run npm run theme:generate; do not edit. */\nexport const GENERATED_TOKEN_CATEGORIES = Object.freeze({\n${categories.join('\n')}\n});\n\n${declarations.join('\n')}\n\nexport const GENERATED_TOKEN_INVENTORY = Object.freeze(${JSON.stringify(inventory, null, 2)} as const);\n`;

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
  const outputs = { 'tokens.css': renderTheme(config), 'theme-values.ts': renderThemeValues(config), 'typography.generated.ts': renderTypographyTypes(config), '../ui/typography-scope/typography-scope.generated.module.css': renderTypographyScope(config) };
  for (const [name, content] of Object.entries(outputs)) {
    const output = new URL(name, root);
    if (process.argv.includes('--check')) {
      if (readFileSync(output, 'utf8') !== content) throw new Error(`${name} is stale; run npm run theme:generate`);
    } else writeFileSync(output, content);
  }
  if (process.argv.includes('--check')) console.log('Central theme configuration and generated CSS/host values agree');
}
