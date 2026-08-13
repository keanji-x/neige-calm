import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { assertRouteCardinality, generateMockFiles, parsePathTemplate, validateNoManualPathDispatch, validateOpenApi } from './generator';
import { NEGATIVE_FIXTURES, POSITIVE_FIXTURES, compareFixtureManifest } from './fixture-manifest.mjs';

const fixtureRoot = resolve(import.meta.dirname, 'fixtures');
const POSITIVE = POSITIVE_FIXTURES;
const NEGATIVE = NEGATIVE_FIXTURES;
const EXPECTED_RULES = Object.freeze({
  'broken-ref.json': ['reference'],
  'invalid-template.json': ['path-template'],
  'mismatched-parameters.json': ['path-parameter', 'path-parameter'],
  'no-leading-slash.json': ['path-template'],
  'no-responses.json': ['responses'],
  'unmatched-close.json': ['path-template'],
} as const);

const load = (kind: 'positive' | 'negative', name: string): unknown => JSON.parse(readFileSync(resolve(fixtureRoot, kind, name), 'utf8'));

describe('mock OpenAPI generator', () => {
  it('keeps fixture manifests aligned with every tracked fixture', () => {
    expect(Object.keys(EXPECTED_RULES).sort()).toEqual([...NEGATIVE]);
    const tracked = [...POSITIVE.map((name) => `tools/mock/fixtures/positive/${name}`),
      ...NEGATIVE.map((name) => `tools/mock/fixtures/negative/${name}`)];
    expect(compareFixtureManifest('positive', POSITIVE, tracked)).toBe('');
    expect(compareFixtureManifest('negative', NEGATIVE, tracked)).toBe('');
    expect(compareFixtureManifest('positive', POSITIVE, [...tracked, 'tools/mock/fixtures/positive/unlisted.json']))
      .toContain('manifest differs from Git');
  });

  for (const name of POSITIVE) it(`accepts ${name}`, () => {
    const document = load('positive', name);
    expect(validateOpenApi(document)).toEqual([]);
    expect(generateMockFiles(document, 'export type Cove = {};\nexport interface A {}\n')[0].content).toContain('mockOperations');
  });

  for (const name of Object.keys(EXPECTED_RULES) as Array<keyof typeof EXPECTED_RULES>) it(`attributes every violation in ${name}`, () => {
    const violations = validateOpenApi(load('negative', name));
    const rules = violations.map((item) => item.rule);
    expect(violations).toHaveLength(EXPECTED_RULES[name].length);
    expect(rules.sort()).toEqual([...EXPECTED_RULES[name]].sort());
    expect(violations.every((item) => item.location.length > 0)).toBe(true);
  });

  it('reports both directions of path-parameter mismatch', () => {
    expect(validateOpenApi(load('negative', 'mismatched-parameters.json')).map((item) => item.message)).toEqual([
      '{id} is not declared', 'other is declared but absent from template',
    ]);
  });

  const generatedValue = (content: string, exportName: string): unknown => JSON.parse(
    content.match(new RegExp(`${exportName} = ([\\s\\S]*?) as const;`))?.[1] ?? 'null',
  );

  it('deeply emits path parameters, referenced responses, wire matches, and stable ordering', () => {
    const content = generateMockFiles(load('positive', 'path-and-ref.json'), 'export type Cove = {};\nexport interface Other {}\n')[0].content;
    expect({ operations: generatedValue(content, 'mockOperations'), wireTypes: generatedValue(content, 'schemaWireTypes') }).toEqual({
      operations: [{
        method: 'GET', path: '/api/coves/{coveId}',
        template: [{ kind: 'literal', value: '/api/coves/' }, { kind: 'parameter', name: 'coveId' }],
        parameters: [
          { in: 'path', name: 'coveId', required: true, schema: { $ref: '#/components/schemas/Id' } },
          { in: 'query', name: 'q', required: false, schema: { type: 'string' } },
        ],
        responses: [{ bodies: [{ contentType: 'application/json', schema: { $ref: '#/components/schemas/Cove' } }], status: '200' }],
      }],
      wireTypes: { Cove: 'Cove', Id: null },
    });
  });

  it('fails closed when a required OpenAPI schema loses its wire type mapping', () => {
    const document = { paths: { '/coves': { get: { responses: { 200: { content: { 'application/json': {
      schema: { $ref: '#/components/schemas/Cove' },
    } } } } } } }, components: { schemas: { Cove: { type: 'object' } } } };
    expect(() => generateMockFiles(document, 'export interface CoveRenamed {}'))
      .toThrow('response schema wire types missing: Cove');
    expect(() => generateMockFiles(document, 'export interface Cove {}')).not.toThrow();
  });

  it('recursively requires wire types for schemas nested behind response refs', () => {
    const document = { paths: { '/nested': { get: { responses: { 200: { content: { 'application/json': {
      schema: { $ref: '#/components/schemas/Envelope' },
    } } } } } } }, components: { schemas: {
      Envelope: { type: 'object', properties: { inner: { $ref: '#/components/schemas/MissingInner' } } },
      MissingInner: { type: 'object' },
    } } };
    expect(() => generateMockFiles(document, 'export interface Envelope {}'))
      .toThrow('response schema wire types missing: MissingInner');
  });

  it('continues recursively through an exempt response schema', () => {
    const document = { paths: { '/nested': { get: { responses: { 200: { content: { 'application/json': {
      schema: { $ref: '#/components/schemas/WaveDetail' },
    } } } } } } }, components: { schemas: {
      WaveDetail: { type: 'object', properties: { inner: { $ref: '#/components/schemas/MissingInner' } } },
      MissingInner: { type: 'object' },
    } } };
    expect(() => generateMockFiles(document, ''))
      .toThrow('response schema wire types missing: MissingInner');
  });

  it('serializes object keys in stable code-point order', () => {
    const content = generateMockFiles(load('positive', 'path-and-ref.json'), 'export type Cove = {};')[0].content;
    const [route] = generatedValue(content, 'mockOperations') as Array<Record<string, unknown>>;
    expect(Object.keys(route)).toEqual(['method', 'parameters', 'path', 'responses', 'template']);
  });

  it('fails closed on unknown path-item keys and preserves operation/route cardinality', () => {
    const document = { paths: { '/api/a': { GET: { responses: { 200: {} } } }, '/api/b': { $ref: '#/components/pathItems/P' } } };
    expect(validateOpenApi(document).map(({ rule }) => rule)).toEqual(['path-item-key', 'unsupported-path-item-ref']);
    expect(validateOpenApi({ paths: { '/a': { 'x-owner': 'mock', get: { responses: { 200: {} } } } } })).toEqual([]);
    const valid = { paths: { '/a': { get: { responses: { 200: {} } }, post: { responses: { 204: {} } } } } };
    expect(generatedValue(generateMockFiles(valid, '')[0].content, 'mockOperations')).toHaveLength(2);
    expect(() => assertRouteCardinality(valid.paths, 1)).toThrow('2 input operations, 1 generated routes');
  });

  it('applies operation parameter overrides by exact name and location', () => {
    const document = { paths: { '/a/{id}': { parameters: [{ name: 'id', in: 'path', schema: { type: 'string' } }], get: {
      parameters: [{ name: 'id', in: 'path', schema: { type: 'integer' } }], responses: { 200: {} },
    } } } };
    const operations = generatedValue(generateMockFiles(document, '')[0].content, 'mockOperations') as Array<{ parameters: unknown }>;
    expect(operations[0].parameters).toEqual([{ in: 'path', name: 'id', required: false, schema: { type: 'integer' } }]);
  });

  it('uses code-point ordering for response and content keys', () => {
    const document = { paths: { '/a': { get: { responses: {
      '2xx': { content: { 'application/json': {}, 'application/JSON': {} } }, '2XX': {},
    } } } } };
    const operations = generatedValue(generateMockFiles(document, '')[0].content, 'mockOperations') as Array<{
      responses: Array<{ status: string; bodies: Array<{ contentType: string }> }>;
    }>;
    expect(operations[0].responses.map(({ status }) => status)).toEqual(['2XX', '2xx']);
    expect(operations[0].responses[1].bodies.map(({ contentType }) => contentType)).toEqual(['application/JSON', 'application/json']);
  });

  it('rejects adjacent parameters, queries, fragments, and ambiguous route shapes', () => {
    for (const path of ['/api/{a}{b}', '/api/z?q=1', '/api/z#f']) expect(() => parsePathTemplate(path)).toThrow();
    expect(parsePathTemplate('/files/{file name}').parameters).toEqual(['file name']);
    const document = { paths: {
      '/api/x/{a}': { get: { parameters: [{ name: 'a', in: 'path' }], responses: { 200: {} } } },
      '/api/x/{b}': { get: { parameters: [{ name: 'b', in: 'path' }], responses: { 200: {} } } },
    } };
    expect(validateOpenApi(document).map(({ rule }) => rule)).toEqual(['path-template-ambiguity']);
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
