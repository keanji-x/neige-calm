import { readdirSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { generateMockFiles, parsePathTemplate, validateNoManualPathDispatch, validateOpenApi } from './generator';

const fixtureRoot = resolve(import.meta.dirname, 'fixtures');
const POSITIVE = Object.freeze(['additional-properties.json', 'compositions.json', 'path-and-ref.json'] as const);
const NEGATIVE = Object.freeze(['broken-ref.json', 'invalid-template.json', 'mismatched-parameters.json', 'no-leading-slash.json', 'no-responses.json', 'unmatched-close.json'] as const);
const EXPECTED_RULES = Object.freeze({
  'broken-ref.json': ['reference', 'path-parameter'],
  'invalid-template.json': ['path-template'],
  'mismatched-parameters.json': ['path-parameter'],
  'no-leading-slash.json': ['path-template'],
  'no-responses.json': ['responses'],
  'unmatched-close.json': ['path-template'],
} as const);

const load = (kind: 'positive' | 'negative', name: string): unknown => JSON.parse(readFileSync(resolve(fixtureRoot, kind, name), 'utf8'));

describe('mock OpenAPI generator', () => {
  it('keeps fixture directories exactly equal to their independent manifests', () => {
    expect(readdirSync(resolve(fixtureRoot, 'positive')).sort()).toEqual([...POSITIVE]);
    expect(readdirSync(resolve(fixtureRoot, 'negative')).sort()).toEqual([...NEGATIVE]);
    expect(Object.keys(EXPECTED_RULES).sort()).toEqual([...NEGATIVE]);
  });

  for (const name of POSITIVE) it(`accepts ${name}`, () => {
    const document = load('positive', name);
    expect(validateOpenApi(document)).toEqual([]);
    expect(generateMockFiles(document, 'export type Cove = {};\nexport interface A {}\n')[0].content).toContain('mockOperations');
  });

  for (const name of NEGATIVE) it(`attributes every violation in ${name}`, () => {
    const violations = validateOpenApi(load('negative', name));
    const rules = new Set(violations.map((item) => item.rule));
    for (const rule of EXPECTED_RULES[name]) expect(rules).toContain(rule);
    expect(violations.every((item) => item.location.length > 0)).toBe(true);
  });

  it('reports both directions of path-parameter mismatch', () => {
    expect(validateOpenApi(load('negative', 'mismatched-parameters.json')).map((item) => item.message)).toEqual([
      '{id} is not declared', 'other is declared but absent from template',
    ]);
  });

  it('preserves compositions, nullable maps, content types, and status patterns in output', () => {
    const compositions = generateMockFiles(load('positive', 'compositions.json'), 'export interface A {}')[0].content;
    const maps = generateMockFiles(load('positive', 'additional-properties.json'), '')[0].content;
    expect(compositions).toContain('"oneOf"');
    expect(compositions).toContain('"anyOf"');
    expect(compositions).toContain('"allOf"');
    expect(compositions).toContain('"2XX"');
    expect(compositions).toContain('"default"');
    expect(maps).toContain('"additionalProperties"');
    expect(maps).toContain('"text/plain"');
  });

  it('tokenizes templates instead of flattening them into substring matching', () => {
    expect(parsePathTemplate('/api/coves/{coveId}/cards/{card_id}')).toEqual({
      tokens: [{ kind: 'literal', value: '/api/coves/' }, { kind: 'parameter', name: 'coveId' },
        { kind: 'literal', value: '/cards/' }, { kind: 'parameter', name: 'card_id' }],
      parameters: ['coveId', 'card_id'],
    });
  });

  it('defines the PR2 checkpoint against handwritten path-template dispatch', () => {
    expect(validateNoManualPathDispatch({
      'mock/generated/operations.ts': "const generated = '/api/coves/{id}'",
      'mock/scenarios/edge.ts': "const scenario = '/api/coves/{id}'",
      'mock/adapter.ts': "const handwritten = '/api/coves/{id}'",
    })).toEqual([{ rule: 'no-manual-path-dispatch', location: 'mock/adapter.ts', message: 'path template /api/coves/{id} must come from mock/generated' }]);
  });
});
