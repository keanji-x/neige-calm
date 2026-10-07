// @vitest-environment node
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { renderTheme, renderThemeValues } from './generate-theme.mjs';

type Configuration = {
  typography: { sizes: Record<string, number>; lines: Record<string, number>; tracking: Record<string, number>; roles: Record<string, Record<string, string>> };
  shared: Record<string, string>;
  dark: Record<string, string>;
  terminal: { light: Record<string, string>; dark: Record<string, string> };
};
function source(): Configuration {
  return JSON.parse(readFileSync(new URL('../../web/src/styles/theme.config.json', import.meta.url), 'utf8')) as Configuration;
}

describe('central theme generation', () => {
  it('keeps the shipped stylesheet equal to the real generator output', () => {
    expect(renderTheme(source())).toBe(readFileSync(new URL('../../web/src/styles/tokens.css', import.meta.url), 'utf8'));
  });

  it('keeps generated font and terminal values in sync with the same config', () => {
    expect(renderThemeValues(source())).toBe(readFileSync(new URL('../../web/src/styles/theme-values.ts', import.meta.url), 'utf8'));
  });

  it('changes both role consumers and legacy aliases from one size setting', () => {
    const config = source();
    config.typography.sizes.ui = 15;
    const css = renderTheme(config);
    expect(css).toContain('--text-base: 0.9375rem;');
    expect(css).toContain('--type-ui: 400 0.9375rem/var(--line-ui) var(--font-sans);');
    expect(css).toContain('--type-action: 500 0.9375rem/var(--line-ui) var(--font-sans);');
  });

  it('uses a valid length when normal tracking is changed to a nonzero value', () => {
    const config = source();
    config.typography.tracking.normal = .5;
    expect(renderTheme(config)).toContain('--tracking-normal: 0.03125rem;');
  });

  it('rejects omitted and misspelled role names before emitting CSS', () => {
    const config = source();
    const title = config.typography.roles['page-title'];
    delete config.typography.roles['page-title'];
    expect(() => renderTheme(config)).toThrow('Unexpected roles fields');
    config.typography.roles['page-titel'] = title;
    expect(() => renderTheme(config)).toThrow('Unexpected roles fields');
  });

  it('rejects a misspelled terminal color name and invalid color value', () => {
    const config = source();
    config.terminal.dark.foregroudn = 'red';
    expect(() => renderThemeValues(config)).toThrow('Unexpected terminal.dark fields');
    delete config.terminal.dark.foregroudn;
    config.terminal.dark.foreground = 'totally-invalid';
    expect(() => renderThemeValues(config)).toThrow('Invalid terminal color foreground');
  });

  it('rejects a misspelled role reference instead of generating an invalid font', () => {
    const config = source();
    config.typography.roles.chapter.size = 'headng';
    expect(() => renderTheme(config)).toThrow('references an unknown token');
  });

  it('rejects unknown settings and a line that clips its text', () => {
    const config = source();
    config.typography.roles.ui.colour = 'red';
    expect(() => renderTheme(config)).toThrow('Unexpected ui fields');
    delete config.typography.roles.ui.colour;
    config.typography.lines.heading = 16;
    expect(() => renderTheme(config)).toThrow('line height is smaller');
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
