import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { auditLayeredCss, compareGlobalClassManifest, extractGlobalClasses, layerOrder } from './audit';

const fixtures = resolve(import.meta.dirname, 'fixtures');
const read = (path: string): string => readFileSync(resolve(fixtures, path), 'utf8');
const order = layerOrder(read('entry.css'));

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
