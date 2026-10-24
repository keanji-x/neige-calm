// @vitest-environment node
import { readFileSync } from 'node:fs';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';
import { renderTheme, renderThemeValues, renderTypographyScope, renderTypographyTypes } from './generate-theme.mjs';

type Step = { size: number; line: number; tracking: number };
type Recipe = { family: string; step: string; weight: string };
type Configuration = {
  typography: { families: Record<string, string>; scale: Record<string, Step>; exceptions: Record<string, Step>; roles: Record<string, Recipe | string> };
  navigation: Record<string, string | number>;
  spacing: Record<string, number>;
  shared: Record<string, string>;
  dark: Record<string, string>;
  terminal: { light: Record<string, string>; dark: Record<string, string> };
};
function source(): Configuration {
  return JSON.parse(readFileSync(new URL('../../web/src/styles/theme.config.json', import.meta.url), 'utf8')) as Configuration;
}

describe('central theme generation', () => {
  it('keeps the explicit typography scope generated without palette or spacing', () => {
    const scope = renderTypographyScope(source());
    expect(scope).not.toContain('--bg:');
    expect(scope).not.toContain('--space-');
    expect(scope).toContain('--font-numeric: var(--font-sans);');
  });
  it('changes paired role fonts, tracking and published fields from one step', () => {
    const config = source();
    config.typography.scale['2'] = { size: 15, line: 22, tracking: .01 };
    const css = renderTheme(config);
    expect(css).toContain('--type-ui: 400 0.9375rem/1.375rem var(--font-sans);');
    expect(css).toContain('--type-table: 400 0.9375rem/1.375rem var(--font-sans);');
    expect(css).toContain('--type-subheading-small: 600 0.9375rem/1.375rem var(--font-serif);');
    expect(css).toContain('--type-ui-tracking: 0.01em;');
    expect(css).toContain('--type-ui-size: 0.9375rem;');
    expect(css).toContain('--type-ui-leading: 1.4666666666666666;');
  });
  it('derives vendor fields from the reading role references including ratio one', () => {
    const config = source();
    config.typography.roles.reading = { family: 'code', step: '2', weight: 'regular' };
    config.typography.scale['2'] = { size: 20, line: 20, tracking: 0 };
    const css = renderTheme(config);
    expect(css).toContain('--type-reading-family: var(--font-mono);');
    expect(css).toContain('--type-reading-size: 1.25rem;');
    expect(css).toContain('--type-reading-leading: 1;');
  });
  it('validates and normalizes aliases in the single semantic-role registry', () => {
    const config = source();
    config.typography.roles.reading = 'missing-role';
    expect(() => renderTheme(config)).toThrow('Unknown role reference');
    config.typography.roles.reading = 'detail';
    expect(renderTheme(config)).toContain('--type-reading: 400 1rem/1.5rem var(--font-sans);');
  });
  it.each([
    { first: 'ui', second: 'first' },
    { first: 'first' },
    { first: 'second', second: 'first' },
  ])('rejects aliases without a concrete target: %j', aliases => {
    const config = source();
    Object.assign(config.typography.roles, aliases);
    for (const emit of [renderTheme, renderTypographyTypes, renderTypographyScope, renderThemeValues]) {
      expect(() => emit(config)).toThrow('Alias must reference a concrete recipe');
    }
  });
  it('rejects missing consumed semantics and duplicate role recipes before emission', () => {
    const config = source();
    delete config.typography.roles.ui;
    expect(() => renderTheme(config)).toThrow('Missing required semantic role ui');
    const duplicate = source();
    duplicate.typography.roles['extra-body'] = { family: 'ui', step: '2', weight: 'regular' };
    expect(() => renderTheme(duplicate)).toThrow('Duplicate role recipe');
  });
  it('generates the closed public types and inventory from the editable registry', () => {
    expect(renderTypographyTypes(source())).toBe(readFileSync(new URL('../../web/src/styles/typography.generated.ts', import.meta.url), 'utf8'));
    const config = source();
    config.typography.roles['fixture-role'] = 'reading';
    expect(renderTypographyTypes(config)).toContain('"--type-fixture-role"');
    expect(renderTypographyScope(config)).toContain('--type-fixture-role:');
  });
  it.each(['size', 'family', 'line', 'leading', 'tracking'])('rejects a role colliding with the emitted %s field', field => {
    const config = source();
    config.typography.roles[`ui-${field}`] = 'ui';
    for (const emit of [renderTheme, renderTypographyTypes, renderTypographyScope, renderThemeValues]) {
      expect(() => emit(config)).toThrow(`Duplicate token --type-ui-${field}`);
    }
  });
  it.each(['size', 'family', 'line', 'leading', 'tracking'])('classifies a noncolliding %s-suffixed role by its emission kind', async field => {
    const config = source();
    const role = `fixture-${field}`;
    config.typography.roles[role] = 'ui';
    const generated = renderTypographyTypes(config);
    const javascript = ts.transpileModule(generated, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText;
    const output = await import(`data:text/javascript;base64,${Buffer.from(javascript).toString('base64')}`) as {
      GENERATED_TOKEN_CATEGORIES: Record<string, readonly string[]>; GENERATED_TOKEN_INVENTORY: readonly string[];
    };
    expect(output.GENERATED_TOKEN_CATEGORIES.TypographyToken).toContain(`--type-${role}`);
    for (const category of ['TypeScaleToken', 'LeadingToken', 'TrackingToken', 'FontProjectionToken']) {
      expect(output.GENERATED_TOKEN_CATEGORIES[category]).not.toContain(`--type-${role}`);
    }
    expect(new Set(output.GENERATED_TOKEN_INVENTORY).size).toBe(output.GENERATED_TOKEN_INVENTORY.length);
    const scope = renderTypographyScope(config);
    expect(scope).toContain(`--type-${role}:`);
    expect(scope).toContain(`--type-${role}-family:`);
    expect(scope).not.toContain(`--type-${role}-size:`);

  });
  it('preserves the independently pinned interface, reading and editor font stacks', () => {
    expect(source().typography.families).toEqual({
      ui: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", sans-serif',
      reading: '"New York", "Iowan Old Style", Charter, Georgia, "Songti SC", SimSun, "Noto Serif CJK SC", serif',
      code: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
    });
  });
  it('pins the Radix paired scale independently of generated artifacts', () => {
    const steps = Object.values(source().typography.scale);
    expect(steps.map(({ size, line }) => [size, line])).toEqual([
      [12, 16], [14, 20], [16, 24], [18, 26], [20, 28], [24, 30], [28, 36], [35, 40], [60, 60],
    ]);
    expect(steps.map(({ tracking }) => tracking)).toEqual([.0025, 0, 0, -.0025, -.005, -.00625, -.0075, -.01, -.025]);
    expect(Object.values(source().spacing)).toEqual([0, 4, 8, 12, 16, 24, 32, 40, 48, 64, 1, 2]);
  });
  it('rejects a misspelled terminal color name and invalid color value', () => {
    const config = source();
    config.terminal.dark.foregroudn = 'red';
    expect(() => renderThemeValues(config)).toThrow('Unexpected terminal.dark fields');
    delete config.terminal.dark.foregroudn;
    config.terminal.dark.foreground = 'totally-invalid';
    expect(() => renderThemeValues(config)).toThrow('Invalid terminal color foreground');
  });
  it('rejects unknown role references and legacy independently maintained sizes', () => {
    const config = source();
    config.typography.roles.chapter = { family: 'reading', step: 'unknown', weight: 'bold' };
    expect(() => renderTheme(config)).toThrow('Unknown step reference');
    const legacy = { ...source(), typography: { ...source().typography, sizes: { ui: 14 } } };
    expect(() => renderTheme(legacy)).toThrow();
    const legacyProfiles = { ...source(), typography: { ...source().typography, profiles: {} } };
    expect(() => renderTheme(legacyProfiles)).toThrow();
  });
  it('rejects unknown settings, missing steps and a line that clips its text', () => {
    const config = source();
    const invalidRole = { ...config, typography: { ...config.typography, roles: { ...config.typography.roles,
      ui: { family: 'ui', step: '2', weight: 'regular', colour: 'red' } } } };
    expect(() => renderTheme(invalidRole)).toThrow();
    config.typography.scale['5'].line = 16;
    expect(() => renderTheme(config)).toThrow('line height is smaller');
    delete config.typography.scale['5'];
    expect(() => renderTheme(config)).toThrow('Scale requires all nine steps');
  });
  it('resolves navigation whitespace from spacing and rejects a missing spacing reference', () => {
    const config = source();
    config.spacing['5'] = 25;
    expect(renderTheme(config)).toContain('--navigation-section-gap: var(--space-5);');
    expect(renderTheme(config)).toContain('--space-5: 25px;');
    config.navigation.sectionGap = 'missing';
    expect(() => renderTheme(config)).toThrow('references an unknown spacing step');
  });
  it('rejects theme inventory drift and duplicate value ownership', () => {
    const config = source();
    config.dark['--unknown'] = 'red';
    expect(() => renderTheme(config)).toThrow('Light/dark token inventory differs');
    delete config.dark['--unknown'];
    config.shared['--font-sans'] = 'serif';
    expect(() => renderTheme(config)).toThrow('Duplicate token --font-sans');
  });
});
