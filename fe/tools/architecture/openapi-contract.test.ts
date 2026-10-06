import { readFileSync } from 'node:fs';
import { compileTemplateInputs } from '../../core/domain/template-input.ts';
import { describe, expect, it } from 'vitest';

type JsonObject = Record<string, unknown>;

const METHODS = new Set(['delete', 'get', 'head', 'options', 'patch', 'post', 'put', 'trace']);
const PATH_ITEM_FIELDS = new Set([...METHODS, 'summary', 'description', 'servers', 'parameters']);
/* Response schemas with no `wire.ts` counterpart: `gen:api` exports from calm-types only, so a DTO
 * declared in a calm-server route module is typed by a hand-written zod schema in `core/domain/**` instead. */
const RESPONSE_WIRE_EXCEPTIONS = new Set([
  'ErrorBody', 'GetPlannerRunResponse', 'GitDiffResponse', 'GitStatusResponse',
  'InterruptPlannerCardResponse', 'ListdirResponse', 'ModelsResponse', 'PluginDetail', 'PluginListItem',
  'PlannerInputMutationResponse', 'PlannerInputStaleBody',
  'PlannerSteerResponse', 'PlannerSteerConflictBody',
  'ReadFileResponse', 'ReportBlockWriteResponse',
  'ResetPlannerCardResponse', 'SendPlannerInputResponse',
  'SetPlannerModelResponse',
  // Route-owned DTO decoded by compactPlannerOperation in core/domain/conversation.ts.
  'CompactPlannerResponse',
  'SettingsBag', 'Terminal',
  'ThreadCardResolution', 'TodayLaunchpad', 'TodayLaunchpadReportReset', 'TodayLaunchpadResolved',
  'TodaySummaryStarted',
  'VersionInfo',
  'ViewCatalogEntry', 'TrackBacklinksResponse', 'TrackDetail', 'TrackFsContent', 'TrackFsEntry',
  'TrackReportReadResponse', 'TrackTemplate',
  // Read-only server DTO, decoded by core/domain/template.ts; its shape is pinned below.
  'TrackTemplateDetail',
  'TemplatePluginGuide',
  // Builtin-owned DTO decoded by core/domain/calendar.ts.
  'CalendarEntry',
  // Login answer, decoded by `sessionIdentitySchema` in core/api/auth.ts.
  'WhoamiBody',
  'ReportSeriesResolved', 'ReportSeriesRevConflict', 'TrackPreviews',
]);

function object(value: unknown): value is JsonObject {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function array(value: unknown): unknown[] {
  return Array.isArray(value) ? value as unknown[] : [];
}

function resolveRef(document: JsonObject, ref: string): unknown {
  if (!ref.startsWith('#/')) throw new Error(`external reference: ${ref}`);
  return ref.slice(2).split('/').reduce<unknown>((value, encoded) => {
    const key = encoded.replaceAll('~1', '/').replaceAll('~0', '~');
    if (!object(value) || !(key in value)) throw new Error(`unresolved reference: ${ref}`);
    return value[key];
  }, document);
}

function parsePath(path: string): { names: string[]; shape: string } {
  if (!path.startsWith('/') || path.includes('?') || path.includes('#')) throw new Error(`invalid path: ${path}`);
  const names: string[] = [];
  let shape = '';
  let literal = '';
  for (let cursor = 0; cursor < path.length;) {
    const character = path[cursor];
    if (character === '}') throw new Error(`unmatched }: ${path}`);
    if (character !== '{') {
      literal += character;
      cursor += 1;
      continue;
    }
    if (shape.endsWith('{}') && literal === '') throw new Error(`adjacent parameters: ${path}`);
    shape += literal;
    literal = '';
    const end = path.indexOf('}', cursor + 1);
    if (end < 0) throw new Error(`unclosed {: ${path}`);
    const name = path.slice(cursor + 1, end);
    if (name === '' || name.includes('{') || names.includes(name)) throw new Error(`invalid parameter: ${path}`);
    names.push(name);
    shape += '{}';
    cursor = end + 1;
  }
  return { names, shape: shape + literal };
}

function resolveParameter(document: JsonObject, value: unknown): JsonObject {
  if (!object(value)) throw new Error('parameter must be an object');
  if (typeof value.$ref !== 'string') return value;
  const resolved = resolveRef(document, value.$ref);
  if (!object(resolved)) throw new Error(`parameter reference is not an object: ${value.$ref}`);
  return resolved;
}

function visitRefs(
  document: JsonObject,
  value: unknown,
  visited = new Set<string>(),
): void {
  if (Array.isArray(value)) {
    value.forEach((item) => visitRefs(document, item, visited));
    return;
  }
  if (!object(value)) return;
  if (typeof value.$ref === 'string') {
    const ref = value.$ref;
    const resolved = resolveRef(document, ref);
    if (!visited.has(ref)) {
      visited.add(ref);
      visitRefs(document, resolved, visited);
    }
  }
  Object.entries(value).forEach(([key, item]) => {
    if (key !== '$ref') visitRefs(document, item, visited);
  });
}

function responseRootRefs(document: JsonObject, responses: JsonObject): string[] {
  const refs: string[] = [];
  for (const rawResponse of Object.values(responses)) {
    const response = object(rawResponse) && typeof rawResponse.$ref === 'string'
      ? resolveRef(document, rawResponse.$ref)
      : rawResponse;
    if (!object(response) || !object(response.content)) continue;
    for (const media of Object.values(response.content)) {
      if (!object(media) || !object(media.schema) || typeof media.schema.$ref !== 'string') continue;
      if (media.schema.$ref.startsWith('#/components/schemas/')) refs.push(media.schema.$ref.slice(21));
    }
  }
  return refs;
}

function validateDocument(document: JsonObject, wireSource: string): void {
  if (!object(document.paths)) throw new Error('paths must be an object');
  const routeShapes = new Set<string>();
  const responseSchemaRefs = new Set<string>();
  for (const [path, rawPathItem] of Object.entries(document.paths)) {
    if (!object(rawPathItem)) throw new Error(`path item must be an object: ${path}`);
    for (const key of Object.keys(rawPathItem)) {
      if (!PATH_ITEM_FIELDS.has(key) && !key.startsWith('x-')) throw new Error(`unknown path item field: ${key}`);
    }
    const template = parsePath(path);
    if (routeShapes.has(template.shape)) throw new Error(`ambiguous route shape: ${path}`);
    routeShapes.add(template.shape);
    const sharedParameters = array(rawPathItem.parameters);
    for (const [method, rawOperation] of Object.entries(rawPathItem)) {
      if (!METHODS.has(method)) continue;
      if (!object(rawOperation)) throw new Error(`operation must be an object: ${method} ${path}`);
      const declared = [...sharedParameters, ...array(rawOperation.parameters)]
        .map((value) => resolveParameter(document, value))
        .filter((value) => value.in === 'path' && typeof value.name === 'string')
        .map((value) => value.name as string);
      expect(new Set(declared)).toEqual(new Set(template.names));
      if (!object(rawOperation.responses) || Object.keys(rawOperation.responses).length === 0) {
        throw new Error(`operation has no responses: ${method} ${path}`);
      }
      responseRootRefs(document, rawOperation.responses).forEach((name) => responseSchemaRefs.add(name));
      visitRefs(document, rawOperation.responses);
    }
  }
  const wireTypes = new Set(Array.from(
    wireSource.matchAll(/^export\s+(?:type|interface)\s+([A-Za-z_$][\w$]*)/gm),
    (match) => match[1],
  ));
  const missing = [...responseSchemaRefs]
    .filter((name) => !wireTypes.has(name) && !RESPONSE_WIRE_EXCEPTIONS.has(name))
    .sort();
  if (missing.length > 0) throw new Error(`response schema wire types missing: ${missing.join(', ')}`);
}

describe('generated OpenAPI integrity', () => {
  it('keeps every operation unambiguous, referenced, complete, and wire-covered', () => {
    const document = JSON.parse(readFileSync(new URL('../../core/api/generated/openapi.json', import.meta.url), 'utf8')) as JsonObject;
    const wire = readFileSync(new URL('../../core/api/generated/wire.ts', import.meta.url), 'utf8');
    validateDocument(document, wire);
  });

  it('rejects malformed templates and nested dangling references', () => {
    for (const path of ['/a/{id}/{id}', '/a/{id', '/a/id}', '/a/{a}{b}', '/a?q=1', '/a#x']) {
      expect(() => parsePath(path)).toThrow();
    }
    const document = {
      paths: {
        '/a': { get: { responses: { 200: { $ref: '#/components/responses/A' } } } },
      },
      components: {
        responses: { A: { content: { 'application/json': { schema: { $ref: '#/components/schemas/Missing' } } } } },
        schemas: {},
      },
    };
    expect(() => validateDocument(document, '')).toThrow('unresolved reference');
  });

  it('rejects an operation without responses', () => {
    expect(() => validateDocument({ paths: { '/a': { get: {} } } }, ''))
      .toThrow('operation has no responses');
  });

  it('rejects a top-level response schema without a wire type', () => {
    const document = {
      paths: {
        '/a': { get: { responses: { 200: { content: { 'application/json': {
          schema: { $ref: '#/components/schemas/MissingWire' },
        } } } } } },
      },
      components: { schemas: { MissingWire: { type: 'object' } } },
    };
    expect(() => validateDocument(document, '')).toThrow('response schema wire types missing: MissingWire');
  });
});

it('pins the server DTO outside the generated wire type roster', () => {
  const document = JSON.parse(readFileSync(new URL('../../core/api/generated/openapi.json', import.meta.url), 'utf8')) as {
    components: { schemas: { TrackTemplateDetail: { properties: Record<string, unknown>; required: string[] } } };
  };
  const schema = document.components.schemas.TrackTemplateDetail;
  expect(Object.keys(schema.properties).sort()).toEqual(['body', 'description', 'id', 'instructions', 'title']);
  expect(schema.required.sort()).toEqual(['body', 'id', 'title']);
});


it('the shipped template form binds to its plugin and keeps merge approval by default', () => {
  const body = readFileSync(new URL('../../../crates/calm-server/templates/builtin/dev.md', import.meta.url), 'utf8');
  const plugin = JSON.parse(readFileSync(new URL('../../../plugins/git-forge/manifest.json', import.meta.url), 'utf8')) as { input_schema: unknown };
  const result = compileTemplateInputs(body, plugin.input_schema, { issue_url: 'https://github.com/owner/repo/issues/12' });
  expect(result.status).toBe('ready');
  if (result.status === 'unsupported') throw new Error('Shipped form is unsupported');
  expect(result.input).toEqual({ issue_url: 'https://github.com/owner/repo/issues/12', repo: 'owner/repo', issue_number: 12, merge_policy: 'ask' });
});


it('the shipped development template accepts no issue while preserving merge hold', () => {
  const body = readFileSync(new URL('../../../crates/calm-server/templates/builtin/dev.md', import.meta.url), 'utf8');
  const plugin = JSON.parse(readFileSync(new URL('../../../plugins/git-forge/manifest.json', import.meta.url), 'utf8')) as { input_schema: unknown };
  const result = compileTemplateInputs(body, plugin.input_schema, {});
  expect(result.status).toBe('ready');
  if (result.status === 'unsupported') throw new Error('Shipped form is unsupported');
  expect(result.input).toEqual({ merge_policy: 'ask' });
});
