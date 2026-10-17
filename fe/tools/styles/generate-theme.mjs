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
function declarations(values, label) {
  return Object.entries(object(values, label)).map(([name, value]) => {
    if (!/^--[a-z][a-z0-9-]*$/.test(name)) throw new Error(`Invalid token ${name}`);
    return `    ${name}: ${text(value, name)};`;
  });
}

function rem(value) { return `${value / 16}rem`; }

function roleProfile(type, name) { return type.profiles[type.roles[name]]; }
function roleStep(type, name) { return { ...type.scale, ...type.exceptions }[roleProfile(type, name).step]; }

function typographyTokens(config) {
  const type = typographySchema.parse(config.typography);
  const familyTokens = { ui: '--font-sans', reading: '--font-serif', code: '--font-mono' };
  const generated = {};
  for (const [name, token] of Object.entries(familyTokens)) generated[token] = text(type.families[name], name);
  for (const [name, key] of Object.entries({ normal: 'regular', medium: 'medium', semibold: 'semibold', bold: 'bold' })) {
    generated[`--weight-${name}`] = String(type.weights[key]);
  }
  for (const name of Object.keys(type.roles)) {
    const profile = roleProfile(type, name), step = roleStep(type, name);
    const family = `var(${familyTokens[profile.family]})`;
    generated[`--type-${name}`] = `${type.weights[profile.weight]} ${rem(step.size)}/${rem(step.line)} ${family}`;
    generated[`--type-${name}-tracking`] = `${step.tracking}em`;
    // Complete paired projections serve partial formatting and the vendor's unitless-leading protocol.
    for (const [field, value] of Object.entries({ family, size: rem(step.size), line: rem(step.line), leading: String(step.line / step.size) })) {
      generated[`--type-${name}-${field}`] = value;
    }
  }
  for (const [name, step] of Object.entries(type.scale)) {
    for (const [field, value] of Object.entries(step)) generated[`--font-step-${name}-${field}`] = field === 'tracking' ? `${value}em` : rem(value);
  }
  return generated;
}

/** Build-time owner: no runtime imports, selector scanning or state overrides. */
export function renderTheme(input) {
  const config = object(input, 'theme');
  if (config.version !== 2) throw new Error('Unsupported theme version');
  keys(config, ['version', 'typography', 'navigation', 'shared', 'light', 'dark', 'hostRgb', 'terminal', 'terminalText', 'spacing'], 'theme');
  const generated = typographyTokens(config);
  keys(config.spacing, ['0', 'px', 'half', '1', '2', '3', '4', '5', '6', '7', '8', '9'], 'spacing');
  for (const [name, value] of Object.entries(config.spacing)) generated[`--space-${name}`] = value === 0 ? '0' : `${number(value, 0, 64, `spacing.${name}`)}px`;
  const nav = object(config.navigation, 'navigation');
  const navLimits = { railWidth: [200, 400], rowHeight: [24, 48], headerHeight: [32, 56], moduleRowHeight: [20, 40], railMix: [0, 100] };
  keys(nav, [...Object.keys(navLimits), 'groupGap', 'sectionGap', 'labelInset'], 'navigation');
  for (const name of Object.keys(navLimits)) number(nav[name], navLimits[name][0], navLimits[name][1], name);
  for (const name of ['groupGap', 'sectionGap', 'labelInset']) if (!Object.hasOwn(config.spacing, nav[name])) throw new Error(`navigation.${name} references an unknown spacing step`);
  if (nav.rowHeight < roleStep(config.typography, 'ui').line) throw new Error('Navigation row clips its text line');
  for (const [name, value] of Object.entries(nav)) {
    if (name === 'railMix') continue;
    generated[`--navigation-${name.replace(/[A-Z]/g, letter => `-${letter.toLowerCase()}`)}`] = Object.hasOwn(navLimits, name) ? rem(value) : `var(--space-${value})`;
  }
  generated['--surface-navigation'] = `color-mix(in srgb, var(--surface-rail) ${nav.railMix}%, var(--bg))`;
  const shared = object(config.shared, 'shared');
  for (const name of Object.keys(generated)) if (Object.hasOwn(shared, name)) throw new Error(`Duplicate token ${name}`);
  const light = object(config.light, 'light'), dark = object(config.dark, 'dark');
  if (Object.keys(light).sort().join() !== Object.keys(dark).sort().join()) throw new Error('Light/dark token inventory differs');
  for (const name of Object.keys(light)) if (Object.hasOwn(shared, name) || Object.hasOwn(generated, name)) throw new Error(`Themed token overlaps shared token ${name}`);
  return `/* Generated from theme.config.json. Run npm run theme:generate; do not edit. */\n@layer tokens {\n  :root {\n${declarations({ ...shared, ...generated, ...light }, 'root').join('\n')}\n  }\n\n  [data-theme="dark"] {\n${declarations(dark, 'dark').join('\n')}\n  }\n}\n`;
}

/** Recompute font-dependent roles only at an explicit inherited-family scope. */
export function renderTypographyScope(input) {
  renderTheme(input);
  const fonts = typographyTokens(input);
  const values = {};
  for (const name of Object.keys(input.typography.roles)) values[`--type-${name}`] = fonts[`--type-${name}`];
  for (const [name, value] of Object.entries(fonts)) if (name.endsWith('-family')) values[name] = value;
  for (const name of ['--font-display', '--font-numeric', '--font-code']) values[name] = input.shared[name];
  return `/* Generated from theme.config.json. Run npm run theme:generate; do not edit. */\n@layer ui {\n  .scope {\n${declarations(values, 'typography scope').join('\n')}\n  }\n}\n`;
}

/** Closed generated type families; no hand-maintained role or field inventory. */
export function renderTypographyTypes(input) {
  renderTheme(input);
  const type = typographyTokens(input);
  const allNames = Object.keys(type);
  const roles = Object.keys(input.typography.roles);
  const groups = {
    TypographyToken: roles.map(name => `--type-${name}`),
    TypeScaleToken: allNames.filter(name => name.endsWith('-size')),
    LeadingToken: ['--leading-none', ...allNames.filter(name => /-(?:line|leading)$/.test(name))],
    TrackingToken: allNames.filter(name => name.endsWith('-tracking')),
    FontProjectionToken: allNames.filter(name => name.endsWith('-family')),
    SpacingToken: Object.keys(input.spacing).map(name => `--space-${name}`),
    NavigationToken: Object.keys(input.navigation).filter(name => name !== 'railMix').map(name => `--navigation-${name.replace(/[A-Z]/g, letter => `-${letter.toLowerCase()}`)}`),
  };
  const declarations = Object.entries(groups).map(([name, values]) => `export type ${name} =\n  ${values.map(value => JSON.stringify(value)).join(' | ')};`);
  const inventory = Object.values(groups).flat().sort();
  return `/* Generated from theme.config.json. Run npm run theme:generate; do not edit. */\n${declarations.join('\n\n')}\n\nexport const GENERATED_TOKEN_INVENTORY = Object.freeze(${JSON.stringify(inventory, null, 2)} as const);\n`;
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
