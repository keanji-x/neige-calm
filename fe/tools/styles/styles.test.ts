import { readdirSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import stylelint from 'stylelint';
import { describe, expect, it } from 'vitest';
import { auditLayeredCss, auditRuntimeStyles, compareGlobalClassManifest, extractGlobalClasses, layerOrder, STYLE_RULES, type RuntimeDocument } from './audit';

const fixtures = resolve(import.meta.dirname, 'fixtures');
const read = (path: string): string => readFileSync(resolve(fixtures, path), 'utf8');
const order = layerOrder(read('entry.css'));
const jsdomModule: unknown = createRequire(import.meta.url)('jsdom');
const JSDOM = (jsdomModule as { JSDOM: new (html: string) => { window: { document: RuntimeDocument } } }).JSDOM;

async function lintCss(code: string, filename: string, exceptions: string[] = []) {
  return stylelint.lint({
    code,
    codeFilename: resolve(import.meta.dirname, '../..', filename),
    config: {
      plugins: ['./tools/styles/stylelint-plugin.mjs'],
      rules: {
        'neige-calm/unlayered-cm-scope': [true, {
          unlayeredExceptions: exceptions,
        }],
      },
    },
  });
}

describe('CSS AST fixtures', () => {
  it('covers exactly every rule the audit can emit', () => {
    const unreadable = {};
    Object.defineProperty(unreadable, 'cssRules', { get: () => { throw new Error('denied'); } });
    const evidence: Record<string, () => { rule: string }[]> = {
      'rule-in-layer': () => auditLayeredCss(read('layered/negative/case.css'), order),
      'known-layer': () => auditLayeredCss('@layer alien { .known {} }', order),
      'unlayered-cm-scope': () => auditLayeredCss(read('unlayered-cm/negative/case.css'), order, true),
      'global-class-manifest': () => compareGlobalClassManifest(extractGlobalClasses([read('manifest/classes.css')]), ['alpha']),
      'runtime-stylesheet-readable': () => auditRuntimeStyles({ styleSheets: [unreadable], querySelectorAll: () => [] }, order),
      'runtime-inline-style': () => auditRuntimeStyles(new JSDOM(read('runtime-inline/negative/page.html')).window.document, order),
    };
    expect(new Set(Object.keys(evidence))).toEqual(new Set(STYLE_RULES));
    expect(new Set(STYLE_RULES)).toEqual(new Set(Object.keys(evidence)));
    for (const [rule, runEvidence] of Object.entries(evidence)) {
      expect(runEvidence().some((violation) => violation.rule === rule), rule).toBe(true);
    }
  });
  it('uses entry.css as the sole layer-order source', () => {
    expect(order).toEqual(['reset', 'vendor', 'tokens', 'base', 'astryx', 'ui', 'features', 'overrides']);
  });

  it('accepts layered rules and rejects an unlayered rule', () => {
    expect(auditLayeredCss(read('layered/positive/case.css'), order)).toEqual([]);
    expect(auditLayeredCss(read('layered/negative/case.css'), order)).toEqual([
      { rule: 'rule-in-layer', message: 'unlayered selector: .loose' },
    ]);
    expect(auditLayeredCss('@layer alien { .known {} }', order)).toEqual([
      { rule: 'known-layer', message: 'unknown layer alien' },
    ]);
    expect(auditLayeredCss('@layer ui.card { .nested {} }', order)).toEqual([]);
  });

  it('covers every supported @layer writing form with the effective top-level name', () => {
    const forms = new Map([
      ['named.css', 'name'],
      ['dotted.css', 'a'],
      ['nested.css', 'a'],
      ['nested-dotted.css', 'a'],
      ['order.css', 'none'],
      ['anonymous.css', 'anonymous'],
      ['import.css', 'name'],
    ]);
    for (const [file] of forms) {
      expect(auditLayeredCss(read(`layer-forms/${file}`), ['name', 'a'] as const), file).toEqual([]);
    }
    const fixtureFiles = new Set(readdirSync(resolve(fixtures, 'layer-forms')));
    expect(fixtureFiles).toEqual(new Set(forms.keys()));
    expect(new Set(forms.keys())).toEqual(fixtureFiles);
  });

  it('limits each exception selector by its rightmost compound', () => {
    expect(auditLayeredCss(read('unlayered-cm/positive/case.css'), order, true)).toEqual([]);
    expect(auditLayeredCss(read('unlayered-cm/negative/case.css'), order, true)).toHaveLength(1);
    expect(auditLayeredCss('.application-panel[data-target=".cm-editor"] {}', order, true)).toHaveLength(1);
    expect(auditLayeredCss('.application-panel:not(.cm-editor) {}', order, true)).toHaveLength(1);
  });

  it('compares extracted global classes with the manifest in both directions', () => {
    const actual = extractGlobalClasses([read('manifest/classes.css')]);
    expect(compareGlobalClassManifest(actual, ['alpha', 'beta'])).toEqual([]);
    expect(compareGlobalClassManifest(actual, ['alpha', 'stale'])).toEqual([
      { rule: 'global-class-manifest', message: 'CSS-only class: beta' },
      { rule: 'global-class-manifest', message: 'manifest-only class: stale' },
    ]);
  });
});

describe('stylelint layer-boundary rule', () => {
  it('does not treat ordinary files as named unlayered exceptions', async () => {
    const result = await lintCss('.loose {}', 'ordinary.css');
    expect(result.errored).toBe(false);
  });

  it('limits named exception files to real rightmost .cm- classes', async () => {
    const filename = 'exception.css';
    expect((await lintCss('.editor > .cm-editor {}', filename, [filename])).errored).toBe(false);
    expect((await lintCss('.panel:not(.cm-editor) {}', filename, [filename])).errored).toBe(true);
  });
});

it('runtime fixture proves an injected unlayered style is reported', () => {
  const positive = new JSDOM(read('runtime/positive/page.html')).window.document;
  const negative = new JSDOM(read('runtime/negative/page.html')).window.document;
  expect(auditRuntimeStyles(positive, order)).toEqual([]);
  expect(auditRuntimeStyles(negative, order)).toEqual([
    { rule: 'rule-in-layer', message: 'unlayered selector: .injected' },
  ]);
});

it('runtime audit also reports inline style attributes', () => {
  const positive = new JSDOM(read('runtime-inline/positive/page.html')).window.document;
  const negative = new JSDOM(read('runtime-inline/negative/page.html')).window.document;
  expect(auditRuntimeStyles(positive, order)).toEqual([]);
  expect(auditRuntimeStyles(negative, order)).toEqual([
    { rule: 'runtime-inline-style', message: 'inline style attribute found' },
  ]);
});

it('runtime audit inspects stylesheet rules not owned by style elements', () => {
  const document: RuntimeDocument = {
    styleSheets: [{ cssRules: [{ cssText: '.external { color: red; }' }] }],
    querySelectorAll: () => [],
  };
  expect(auditRuntimeStyles(document, order)).toEqual([
    { rule: 'rule-in-layer', message: 'unlayered selector: .external' },
  ]);
});

it('runtime audit reports unreadable stylesheet rules', () => {
  const unreadable = {};
  Object.defineProperty(unreadable, 'cssRules', { get: () => { throw new Error('denied'); } });
  const document: RuntimeDocument = { styleSheets: [unreadable], querySelectorAll: () => [] };
  expect(auditRuntimeStyles(document, order)).toEqual([
    { rule: 'runtime-stylesheet-readable', message: 'stylesheet cssRules are not readable' },
  ]);
});

it('runtime CSSOM branch reads rules produced by jsdom CSSOM', () => {
  const real = new JSDOM(read('runtime-cssom/positive/page.html')).window.document;
  const negative = new JSDOM(read('runtime-cssom/negative/page.html')).window.document;
  const firstSheet = Array.from(real.styleSheets)[0];
  expect(Array.from(firstSheet?.cssRules ?? [])[0]?.cssText).toContain('@layer ui');
  const cssomOnly: RuntimeDocument = {
    styleSheets: real.styleSheets,
    querySelectorAll: (selector) => selector === 'style' ? [] : real.querySelectorAll(selector),
  };
  expect(auditRuntimeStyles(cssomOnly, order)).toEqual([]);
  const negativeCssomOnly: RuntimeDocument = {
    styleSheets: negative.styleSheets,
    querySelectorAll: (selector) => selector === 'style' ? [] : negative.querySelectorAll(selector),
  };
  expect(auditRuntimeStyles(negativeCssomOnly, order)).toEqual([
    { rule: 'rule-in-layer', message: 'unlayered selector: .cssom-loose' },
  ]);
});
