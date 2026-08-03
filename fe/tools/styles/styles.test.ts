import { readdirSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import stylelint from 'stylelint';
import { parse } from 'yaml';
import { describe, expect, it } from 'vitest';
import { auditLayeredCss, auditRuntimeStyles, compareGlobalClassManifest, CSS_NODE_SOURCES, extractGlobalClasses, layerOrder, STYLE_RULES, type RuntimeDocument } from './audit';
import { auditCssImports, auditDataAttributes, auditModuleLayer, auditStyleRepository, auditUnlayeredExceptions, EXPECTED_LAYER_ORDER } from './repository-check.mjs';

const fixtures = resolve(import.meta.dirname, 'fixtures');
const read = (path: string): string => readFileSync(resolve(fixtures, path), 'utf8');
const productionEntry = resolve(import.meta.dirname, '../../web/src/styles/entry.css');
const order = layerOrder(readFileSync(productionEntry, 'utf8'));
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
  it('covers the complete layer source x form x position surface in both directions', () => {
    const sources = ['layer-block', 'layer-statement', 'layer-import'] as const;
    const forms = ['named', 'anonymous', 'unlayered'] as const;
    const positions = ['top', 'nested'] as const;
    const applicable = (source: typeof sources[number], form: typeof forms[number], position: typeof positions[number]): boolean =>
      !(source === 'layer-statement' && form !== 'named') &&
      !(source === 'layer-block' && form === 'unlayered') &&
      !(source === 'layer-import' && position === 'nested');
    const expected = new Set(sources.flatMap((source) => forms.flatMap((form) => positions
      .filter((position) => applicable(source, form, position))
      .map((position) => `${source}-${form}-${position}`))));
    const traversalSurface = new Map<string, string>([
      ['layer-block-named-top', 'traversal/layer-block-named-top.css'],
      ['layer-block-named-nested', 'traversal/layer-block-named-nested.css'],
      ['layer-block-anonymous-top', 'traversal/layer-block-anonymous-top.css'],
      ['layer-block-anonymous-nested', 'traversal/layer-block-anonymous-nested.css'],
      ['layer-statement-named-top', 'traversal/layer-statement-named-top.css'],
      ['layer-statement-named-nested', 'traversal/layer-statement-named-nested.css'],
      ['layer-import-named-top', 'traversal/layer-import-named-top.css'],
      ['layer-import-anonymous-top', 'traversal/layer-import-anonymous-top.css'],
      ['layer-import-unlayered-top', 'traversal/layer-import-unlayered-top.css'],
    ]);
    expect(new Set(traversalSurface.keys())).toEqual(expected);
    const auxiliaryFixtures = [
      'layer-child-anonymous.css',
      'layer-child-named.css',
      'runtime-style-cssom.html',
      'static-anonymous-layer.css',
      'static-layer-import.css',
      'static-layer-statement.css',
    ];
    expect(new Set(readdirSync(resolve(fixtures, 'traversal')))).toEqual(new Set([
      ...traversalSurface.values(),
      ...auxiliaryFixtures,
    ].map((fixture) => fixture.replace(/^traversal\//, ''))));
    for (const [cell, fixture] of traversalSurface) {
      expect(auditLayeredCss(read(fixture), order), cell).not.toEqual([]);
    }
  });

  it('covers every static and runtime CSS node source in both directions', () => {
    const traversalSurface = new Map<string, () => unknown>([
      ['static-rule', () => auditLayeredCss(read('layered/negative/case.css'), order)],
      ['static-layer-statement', () => auditLayeredCss(read('traversal/static-layer-statement.css'), order)],
      ['static-layer-import', () => auditLayeredCss(read('traversal/static-layer-import.css'), order)],
      ['static-anonymous-layer', () => auditLayeredCss(read('traversal/static-anonymous-layer.css'), order)],
      ['runtime-style-text', () => auditRuntimeStyles(new JSDOM(read('runtime/negative/page.html')).window.document, order)],
      ['runtime-style-cssom', () => {
        const document = new JSDOM(read('traversal/runtime-style-cssom.html')).window.document;
        (Array.from(document.styleSheets)[0] as { insertRule(rule: string): number } | undefined)?.insertRule('.cssom-injected {}');
        return auditRuntimeStyles(document, order);
      }],
      ['runtime-external-stylesheet', () => auditRuntimeStyles({ styleSheets: [{ cssRules: [{ cssText: '.external {}' }] }], querySelectorAll: () => [] }, order)],
      ['runtime-inline-attribute', () => auditRuntimeStyles(new JSDOM(read('runtime-inline/negative/page.html')).window.document, order)],
    ]);
    expect(new Set(traversalSurface.keys())).toEqual(new Set(CSS_NODE_SOURCES));
    for (const [source, evidence] of traversalSurface) expect(evidence(), source).not.toEqual([]);
  });
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
    for (const [rule, runEvidence] of Object.entries(evidence)) {
      expect(runEvidence().some((violation) => violation.rule === rule), rule).toBe(true);
    }
  });
  it('uses entry.css as the sole layer-order source', () => {
    expect(order).toEqual(EXPECTED_LAYER_ORDER);
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

  it('rejects every at-rule form that introduces a layer outside the order', () => {
    expect(auditLayeredCss(read('traversal/static-layer-statement.css'), order)).toEqual([
      { rule: 'known-layer', message: 'unknown layer alien' },
    ]);
    expect(auditLayeredCss(read('traversal/static-layer-import.css'), order)).toEqual([
      { rule: 'known-layer', message: 'unknown layer alien' },
    ]);
    expect(auditLayeredCss(read('traversal/static-anonymous-layer.css'), order)).toEqual([
      { rule: 'known-layer', message: 'anonymous layer' },
    ]);
  });

  it('requires every import to declare a named layer', () => {
    expect(auditLayeredCss(read('layer-import/positive.css'), order)).toEqual([]);
    expect(auditLayeredCss(read('layer-import/negative.css'), order)).toEqual([
      {
        rule: 'rule-in-layer',
        message: 'imported rules cannot be statically inspected; @import must explicitly declare layer',
      },
    ]);
  });

  it('inherits named child layers but rejects anonymous child layers', () => {
    expect(auditLayeredCss('@layer ui { @layer alien {} }', order)).toEqual([]);
    expect(auditLayeredCss(read('traversal/layer-child-named.css'), order)).toEqual([]);
    expect(auditLayeredCss(read('traversal/layer-child-anonymous.css'), order)).toEqual([
      { rule: 'known-layer', message: 'anonymous layer' },
    ]);
  });

  it('labels an invalid comma-separated block as a layer list', () => {
    expect(auditLayeredCss('@layer ui, alien { .a {} }', order)).toEqual([
      { rule: 'known-layer', message: 'unknown layer list: ui, alien' },
    ]);
  });

  it('makes import.css and order.css real negatives when their layer leaves the order', () => {
    expect(auditLayeredCss(read('layer-forms/import.css'), ['a'])).toEqual([
      { rule: 'known-layer', message: 'unknown layer name' },
    ]);
    expect(auditLayeredCss(read('layer-forms/order.css'), ['a'])).toEqual([
      { rule: 'known-layer', message: 'unknown layer b' },
    ]);
  });

  it('covers every supported @layer writing form with the effective top-level name', () => {
    const forms = new Map<string, string | readonly string[] | null>([
      ['named.css', 'name'],
      ['dotted.css', 'a'],
      ['nested.css', 'a'],
      ['nested-dotted.css', 'a'],
      ['order.css', ['a', 'b']],
      ['anonymous.css', null],
      ['import.css', 'name'],
    ]);
    for (const [file, expected] of forms) {
      const css = read(`layer-forms/${file}`);
      if (expected === null) {
        expect(auditLayeredCss(css, order), file).toEqual([{ rule: 'known-layer', message: 'anonymous layer' }]);
      } else {
        const expectedLayers = typeof expected === 'string' ? [expected] : expected;
        expect(auditLayeredCss(css, expectedLayers), file).toEqual([]);
        expect(auditLayeredCss(css, ['zzz']).some(({ rule }) => rule === 'known-layer'), file).toBe(true);
      }
    }
    const fixtureFiles = new Set(readdirSync(resolve(fixtures, 'layer-forms')));
    expect(fixtureFiles).toEqual(new Set(forms.keys()));
  });

  it('limits each exception selector by its rightmost compound', () => {
    expect(auditLayeredCss(read('unlayered-cm/positive/case.css'), order, true)).toEqual([]);
    expect(auditLayeredCss(read('unlayered-cm/negative/case.css'), order, true)).toHaveLength(1);
    expect(auditLayeredCss('.application-panel[data-target=".cm-editor"] {}', order, true)).toHaveLength(1);
    expect(auditLayeredCss('.application-panel:not(.cm-editor) {}', order, true)).toHaveLength(1);
  });

  it('still rejects unknown layer statements and imports in unlayered exception files', () => {
    expect(auditLayeredCss('@layer alien; .cm-x {}', order, true)).toEqual([
      { rule: 'known-layer', message: 'unknown layer alien' },
    ]);
    expect(auditLayeredCss('@import url("theme.css") layer(alien); .cm-x {}', order, true)).toEqual([
      { rule: 'known-layer', message: 'unknown layer alien' },
    ]);
  });

  it('still rejects unknown and anonymous layer blocks in unlayered exception files', () => {
    expect(auditLayeredCss(read('unlayered-exception-layers/unknown.css'), order, true)).toEqual([
      { rule: 'known-layer', message: 'unknown layer alien' },
    ]);
    expect(auditLayeredCss(read('unlayered-exception-layers/anonymous.css'), order, true)).toEqual([
      { rule: 'known-layer', message: 'anonymous layer' },
    ]);
    expect(auditLayeredCss('@media print { @layer { .cm-x {} } }', order, true)).toEqual([
      { rule: 'known-layer', message: 'anonymous layer' },
    ]);
    expect(auditLayeredCss('.cm-x {}', order, true)).toEqual([]);
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

it('runtime audit reads CSSOM rules inserted into a style-owned sheet', () => {
  const document = new JSDOM(read('traversal/runtime-style-cssom.html')).window.document;
  (Array.from(document.styleSheets)[0] as { insertRule(rule: string): number } | undefined)?.insertRule('.cssom-injected {}');
  expect(auditRuntimeStyles(document, order)).toEqual([
    { rule: 'rule-in-layer', message: 'unlayered selector: .cssom-injected' },
  ]);
});

describe('P8b2 forward style gates', () => {
  it('rejects an unregistered global class against the empty manifest', () => {
    expect(compareGlobalClassManifest(extractGlobalClasses(['.escaped {}']), [])).toEqual([
      { rule: 'global-class-manifest', message: 'CSS-only class: escaped' },
    ]);
  });

  it('rejects a non-prefixed data attribute from the negative fixture', () => {
    expect(auditDataAttributes(read('data-attributes/negative.txt'), 'fixture.tsx')).toEqual([
      'fixture.tsx: nonconforming data-card-id; use data-nc-<kebab-case>',
    ]);
    expect(auditDataAttributes('const x = <div data-nc-card-id="42" aria-label="card" />;', 'ok.tsx')).toEqual([]);
    expect(auditDataAttributes("const x = <div {...{'data-card-id': 1}} />;", 'spread.tsx'))
      .toContain('spread.tsx: nonconforming data-card-id; use data-nc-<kebab-case>');
    expect(auditDataAttributes("el.setAttribute('data-card-id', '1');", 'core/dom.ts'))
      .toContain('core/dom.ts: nonconforming data-card-id; use data-nc-<kebab-case>');
  });

  it('requires every CSS Module to declare its owning layer', () => {
    expect(auditModuleLayer(read('module-layer/negative.module.css'), 'web/src/features/wave/bad.module.css'))
      .toEqual(['web/src/features/wave/bad.module.css: unlayered selector: .unlayered']);
    expect(auditModuleLayer('@layer features { .local {} }', 'web/src/features/wave/good.module.css')).toEqual([]);
    expect(auditModuleLayer('@layer ui { .local {} }', 'web/src/ui/dialog/good.module.css')).toEqual([]);
  });

  it('rejects an unlayered rule in an ordinary non-module stylesheet', () => {
    const fixtureRoot = resolve(fixtures, 'repository/non-module-negative');
    expect(auditStyleRepository(fixtureRoot)).toContain(
      'web/src/features/wave/legacy.css: unlayered selector: button',
    );
  });

  it('audits entry.css imports rather than only reading its layer order', () => {
    expect(auditStyleRepository(resolve(fixtures, 'repository/entry-import-negative'))).toContain(
      'web/src/styles/entry.css: imported rules cannot be statically inspected; @import must explicitly declare layer',
    );
  });

  it('rejects a reversed production layer order', () => {
    expect(auditStyleRepository(resolve(fixtures, 'repository/order-negative'))).toContain(
      `web/src/styles/entry.css: layer order must be ${EXPECTED_LAYER_ORDER.join(' → ')}`,
    );
  });

  it('enforces the single CSS entry and vendor import boundary', () => {
    expect(auditCssImports('@import "./loose.css" layer(ui);', 'web/src/ui/loose.css')).toHaveLength(1);
    expect(auditCssImports(read('css-imports/ts-direct.txt'), 'web/src/main.tsx')).toHaveLength(1);
    expect(auditCssImports("@import '@astryxdesign/core/astryx.css' layer(astryx);", 'web/src/styles/entry.css'))
      .toContain('web/src/styles/entry.css: third-party CSS must be imported from styles/vendor.css');
    expect(auditCssImports("@import '@astryxdesign/core/astryx.css' layer(astryx);", 'web/src/styles/vendor.css'))
      .toEqual([]);
  });

  it('binds each unlayered exception to selector, property, expiry, and actual use', () => {
    const css = read('unlayered-exceptions/exact.css');
    const negative = parse(read('unlayered-exceptions/negative.yaml')) as Record<string, {
      selector: string; property: string; expiry: string;
    }[]>;
    expect(auditUnlayeredExceptions(css, 'case.css', order,
      [{ selector: '.editor > .cm-content', property: 'caret-color', expiry: '2099-01-01' }], '2026-08-03'))
      .toEqual([]);
    expect(auditUnlayeredExceptions(css, 'case.css', order,
      negative['wrong-property'] ?? [], '2026-08-03'))
      .toEqual(expect.arrayContaining([
        'case.css: unapproved unlayered declaration .editor > .cm-content { caret-color }',
        'case.css: unused exception .editor > .cm-content { color }',
      ]));
    expect(auditUnlayeredExceptions(css, 'case.css', order,
      negative.expired ?? [], '2026-08-03'))
      .toContain('case.css: exception 1 expired on 2020-01-01');
    expect(auditUnlayeredExceptions(css, 'case.css', order, negative.unused ?? [], '2026-08-03'))
      .toContain('case.css: unused exception .editor > .cm-line { color }');
    expect(auditUnlayeredExceptions(css, 'case.css', order, negative['wrong-selector'] ?? [], '2026-08-03'))
      .toContain('case.css: exception 1 selector lacks rightmost .cm- scope: .editor > .not-cm');
  });

  it('audits the real repository manifests and forward gates', () => {
    expect(auditStyleRepository(resolve(import.meta.dirname, '../..'))).toEqual([]);
  });
});
