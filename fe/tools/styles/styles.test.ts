import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { auditLayeredCss, auditRuntimeStyles, compareGlobalClassManifest, extractGlobalClasses, layerOrder, type RuntimeDocument } from './audit';

const fixtures = resolve(import.meta.dirname, 'fixtures');
const read = (path: string): string => readFileSync(resolve(fixtures, path), 'utf8');
const order = layerOrder(read('entry.css'));
const jsdomModule: unknown = createRequire(import.meta.url)('jsdom');
const JSDOM = (jsdomModule as { JSDOM: new (html: string) => { window: { document: RuntimeDocument } } }).JSDOM;

describe('CSS AST fixtures', () => {
  it('uses entry.css as the sole layer-order source', () => {
    expect(order).toEqual(['reset', 'vendor', 'tokens', 'base', 'astryx', 'ui', 'features', 'overrides']);
  });

  it('accepts layered rules and rejects an unlayered rule', () => {
    expect(auditLayeredCss(read('layered/positive/case.css'), order)).toEqual([]);
    expect(auditLayeredCss(read('layered/negative/case.css'), order)).toEqual([
      { rule: 'rule-in-layer', message: 'unlayered selector: .loose' },
    ]);
  });

  it('limits each exception selector by its rightmost compound', () => {
    expect(auditLayeredCss(read('unlayered-cm/positive/case.css'), order, true)).toEqual([]);
    expect(auditLayeredCss(read('unlayered-cm/negative/case.css'), order, true)).toHaveLength(1);
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

it('runtime fixture proves an injected unlayered style is reported', () => {
  const positive = new JSDOM(read('runtime/positive/page.html')).window.document;
  const negative = new JSDOM(read('runtime/negative/page.html')).window.document;
  expect(auditRuntimeStyles(positive, order)).toEqual([]);
  expect(auditRuntimeStyles(negative, order)).toEqual([
    { rule: 'rule-in-layer', message: 'unlayered selector: .injected' },
  ]);
});
