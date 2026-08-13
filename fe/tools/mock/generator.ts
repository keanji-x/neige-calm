export const HTTP_METHODS = Object.freeze(['delete', 'get', 'head', 'options', 'patch', 'post', 'put', 'trace'] as const);

type JsonObject = Record<string, unknown>;
export interface Violation { rule: string; location: string; message: string }
export interface TemplateLiteralToken { kind: 'literal'; value: string }
export interface TemplateParameterToken { kind: 'parameter'; name: string }
export type TemplateToken = TemplateLiteralToken | TemplateParameterToken;
export interface GeneratedFile { path: string; content: string }

const object = (value: unknown): value is JsonObject => value !== null && typeof value === 'object' && !Array.isArray(value);
const unknownArray = (value: unknown): unknown[] => Array.isArray(value) ? value as unknown[] : [];
const codePointCompare = (left: string, right: string): number => left < right ? -1 : left > right ? 1 : 0;
const PATH_ITEM_FIELDS = new Set<string>([...HTTP_METHODS, 'summary', 'description', 'servers', 'parameters']);
const responseWireTypeExceptionNames = [
  ...['ErrorBody', 'GetSpecRunResponse', 'GitDiffResponse', 'GitStatusResponse', 'InterruptSpecCardResponse',
    'ListdirResponse', 'PluginDetail', 'PluginListItem', 'RatifyCardResponse', 'ReadFileResponse',
    'ReportBlockWriteResponse', 'ResetSpecCardResponse', 'SendSpecInputResponse', 'SettingsBag', 'Terminal',
    'ThreadCardResolution', 'TodayLaunchpad', 'VersionInfo', 'ViewCatalogEntry', 'WaveBacklinksResponse',
    'WaveDetail', 'WaveFsContent', 'WaveFsEntry', 'WaveReportReadResponse']
] as const;
const RESPONSE_WIRE_TYPE_EXCEPTION_LIMIT = 24;
export const RESPONSE_WIRE_TYPE_EXCEPTION_EXPIRY = '2026-12-31';
if (responseWireTypeExceptionNames.length > RESPONSE_WIRE_TYPE_EXCEPTION_LIMIT) {
  throw new Error(`response wire type exceptions may only shrink (maximum ${RESPONSE_WIRE_TYPE_EXCEPTION_LIMIT})`);
}
export const RESPONSE_WIRE_TYPE_EXEMPTIONS: Readonly<Record<string, string>> = Object.freeze(
  responseWireTypeExceptionNames.reduce<Record<string, string>>((entries, name) => {
    entries[name] = `${name} is a legacy response without a frozen ts-rs wire export; migrate by ${RESPONSE_WIRE_TYPE_EXCEPTION_EXPIRY}.`;
    return entries;
  }, {}),
);

export function parsePathTemplate(path: string): { tokens: TemplateToken[]; parameters: string[] } {
  if (!path.startsWith('/')) throw new Error('path template must start with /');
  if (path.includes('?') || path.includes('#')) throw new Error('path template must not contain a query or fragment');
  const tokens: TemplateToken[] = [];
  const parameters: string[] = [];
  let literal = '';
  for (let cursor = 0; cursor < path.length;) {
    const character = path[cursor];
    if (character === '}') throw new Error(`unmatched } at ${cursor}`);
    if (character !== '{') { literal += character; cursor += 1; continue; }
    if (tokens.at(-1)?.kind === 'parameter' && literal === '') throw new Error('adjacent parameters require a literal separator');
    if (literal !== '') { tokens.push({ kind: 'literal', value: literal }); literal = ''; }
    const end = path.indexOf('}', cursor + 1);
    if (end < 0) throw new Error(`unclosed { at ${cursor}`);
    const name = path.slice(cursor + 1, end);
    if (name === '' || name.includes('{')) throw new Error(`invalid parameter name ${JSON.stringify(name)}`);
    if (parameters.includes(name)) throw new Error(`duplicate parameter {${name}}`);
    parameters.push(name);
    tokens.push({ kind: 'parameter', name });
    cursor = end + 1;
  }
  if (literal !== '') tokens.push({ kind: 'literal', value: literal });
  return { tokens, parameters };
}

function resolveLocalRef(document: JsonObject, ref: string): unknown {
  if (!ref.startsWith('#/')) throw new Error(`external reference is unsupported: ${ref}`);
  return ref.slice(2).split('/').reduce<unknown>((value, encoded) => {
    if (!object(value)) throw new Error(`unresolved reference: ${ref}`);
    const key = encoded.replaceAll('~1', '/').replaceAll('~0', '~');
    if (!(key in value)) throw new Error(`unresolved reference: ${ref}`);
    return value[key];
  }, document);
}

function resolveParameter(document: JsonObject, value: unknown): JsonObject {
  if (!object(value)) throw new Error('parameter must be an object');
  if (typeof value.$ref !== 'string') return value;
  const resolved = resolveLocalRef(document, value.$ref);
  if (!object(resolved)) throw new Error(`parameter reference is not an object: ${value.$ref}`);
  return resolved;
}

export function validateOpenApi(input: unknown): Violation[] {
  const violations: Violation[] = [];
  if (!object(input) || !object(input.paths)) return [{ rule: 'document-shape', location: '#', message: 'paths must be an object' }];
  const templateOwners = new Map<string, string>();
  for (const [path, pathItemValue] of Object.entries(input.paths)) {
    if (!object(pathItemValue)) { violations.push({ rule: 'path-item-shape', location: path, message: 'path item must be an object' }); continue; }
    for (const key of Object.keys(pathItemValue)) {
      if (key === '$ref') violations.push({ rule: 'unsupported-path-item-ref', location: path, message: 'path item $ref is not supported until PR2' });
      else if (!PATH_ITEM_FIELDS.has(key) && !key.startsWith('x-')) violations.push({
        rule: 'path-item-key', location: path, message: `unrecognized path item key ${JSON.stringify(key)}`,
      });
    }
    let parsed: ReturnType<typeof parsePathTemplate>;
    try { parsed = parsePathTemplate(path); }
    catch (error) { violations.push({ rule: 'path-template', location: path, message: String(error) }); continue; }
    const skeleton = parsed.tokens.map((token) => token.kind === 'literal' ? token.value : '{}').join('');
    const owner = templateOwners.get(skeleton);
    if (owner !== undefined) violations.push({ rule: 'path-template-ambiguity', location: path, message: `same route shape as ${owner}` });
    else templateOwners.set(skeleton, path);
    for (const method of HTTP_METHODS) {
      const operation = pathItemValue[method];
      if (operation === undefined) continue;
      const location = `${method.toUpperCase()} ${path}`;
      if (!object(operation)) { violations.push({ rule: 'operation-shape', location, message: 'operation must be an object' }); continue; }
      const rawParameters = [...unknownArray(pathItemValue.parameters), ...unknownArray(operation.parameters)];
      const declared: string[] = [];
      for (const raw of rawParameters) {
        try {
          const parameter = resolveParameter(input, raw);
          if (typeof parameter.name === 'string' && parameter.in === 'path') declared.push(parameter.name);
        } catch (error) { violations.push({ rule: 'reference', location, message: String(error) }); }
      }
      for (const name of parsed.parameters) if (!declared.includes(name)) violations.push({ rule: 'path-parameter', location, message: `{${name}} is not declared` });
      for (const name of declared) if (!parsed.parameters.includes(name)) violations.push({ rule: 'path-parameter', location, message: `${name} is declared but absent from template` });
      if (!object(operation.responses) || Object.keys(operation.responses).length === 0) violations.push({ rule: 'responses', location, message: 'operation must declare responses' });
      const visit = (value: unknown): void => {
        if (Array.isArray(value)) { value.forEach(visit); return; }
        if (!object(value)) return;
        if (typeof value.$ref === 'string') {
          try { resolveLocalRef(input, value.$ref); }
          catch (error) { violations.push({ rule: 'reference', location, message: String(error) }); }
        }
        Object.values(value).forEach(visit);
      };
      visit(operation);
    }
  }
  // A parameter $ref is checked once while resolving parameters and again by the general $ref walk.
  // Collapse only byte-identical diagnostics within one operation; distinct messages or locations remain visible.
  return violations.filter((violation, index) => violations.findIndex((candidate) => candidate.rule === violation.rule
    && candidate.location === violation.location && candidate.message === violation.message) === index);
}

export function extractWireTypeNames(source: string): ReadonlySet<string> {
  return new Set(Array.from(source.matchAll(/^export\s+(?:type|interface)\s+([A-Za-z_$][\w$]*)/gm), (match) => match[1]));
}

function stable(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(stable);
  if (!object(value)) return value;
  return Object.fromEntries(Object.keys(value).sort().map((key) => [key, stable(value[key])]));
}

export function assertRouteCardinality(paths: JsonObject, generatedRouteCount: number): void {
  const inputOperationCount = Object.entries(paths).reduce((count, [, pathItem]) => count + (object(pathItem)
    ? Object.keys(pathItem).filter((key) => HTTP_METHODS.includes(key as typeof HTTP_METHODS[number])).length : 0), 0);
  if (generatedRouteCount !== inputOperationCount) throw new Error(
    `route count mismatch: ${inputOperationCount} input operations, ${generatedRouteCount} generated routes`,
  );
}

export function generateMockFiles(input: unknown, wireSource: string): GeneratedFile[] {
  const violations = validateOpenApi(input);
  if (violations.length) throw new Error(violations.map((item) => `${item.rule} ${item.location}: ${item.message}`).join('\n'));
  const document = input as JsonObject & { paths: JsonObject };
  const wireTypes = extractWireTypeNames(wireSource);
  const responseSchemaRefs = new Set<string>();
  const routes: unknown[] = [];
  for (const path of Object.keys(document.paths).sort()) {
    const pathItem = document.paths[path] as JsonObject;
    for (const method of HTTP_METHODS) {
      const operation = pathItem[method];
      if (!object(operation)) continue;
      const parameterMap = new Map<string, JsonObject>();
      for (const item of [...unknownArray(pathItem.parameters), ...unknownArray(operation.parameters)]) {
        const parameter = resolveParameter(document, item);
        parameterMap.set(`${String(parameter.name)}\0${String(parameter.in)}`, parameter);
      }
      const parameters = [...parameterMap.values()];
      const responses = Object.entries(operation.responses as JsonObject).sort(([left], [right]) => codePointCompare(left, right)).map(([status, raw]) => {
        const response = object(raw) && typeof raw.$ref === 'string' ? resolveLocalRef(document, raw.$ref) : raw;
        const content = object(response) && object(response.content) ? response.content : {};
        const visitedRefs = new Set<string>();
        const collectResponseRefs = (value: unknown): void => {
          if (Array.isArray(value)) { value.forEach(collectResponseRefs); return; }
          if (!object(value)) return;
          if (typeof value.$ref === 'string' && value.$ref.startsWith('#/components/schemas/')) {
            const name = value.$ref.slice(21);
            responseSchemaRefs.add(name);
            if (!Object.hasOwn(RESPONSE_WIRE_TYPE_EXEMPTIONS, name) && !visitedRefs.has(value.$ref)) {
              visitedRefs.add(value.$ref);
              collectResponseRefs(resolveLocalRef(document, value.$ref));
            }
          }
          Object.values(value).forEach(collectResponseRefs);
        };
        collectResponseRefs(content);
        return { status, bodies: Object.entries(content).sort(([left], [right]) => codePointCompare(left, right)).map(([contentType, media]) => ({
          contentType, schema: object(media) ? media.schema ?? null : null,
        })) };
      });
      routes.push({ method: method.toUpperCase(), path, template: parsePathTemplate(path).tokens,
        parameters: parameters.map((parameter) => ({ name: parameter.name, in: parameter.in, required: parameter.required === true, schema: parameter.schema ?? null })),
        responses });
    }
  }
  assertRouteCardinality(document.paths, routes.length);
  const componentSchemas = object(document.components) && object(document.components.schemas) ? document.components.schemas : {};
  const staleWireExceptions = Object.keys(componentSchemas).length > RESPONSE_WIRE_TYPE_EXCEPTION_LIMIT
    ? Object.keys(RESPONSE_WIRE_TYPE_EXEMPTIONS).filter((name) => !(name in componentSchemas)) : [];
  if (staleWireExceptions.length > 0) throw new Error(`stale response schema wire type exceptions: ${staleWireExceptions.join(', ')}`);
  const missingWireTypes = [...responseSchemaRefs].filter((name) => !wireTypes.has(name) && !Object.hasOwn(RESPONSE_WIRE_TYPE_EXEMPTIONS, name)).sort();
  if (missingWireTypes.length > 0) throw new Error(`response schema wire types missing: ${missingWireTypes.join(', ')}`);
  const schemaWireTypes = Object.fromEntries(Object.keys(componentSchemas).sort().map((name) => [name, wireTypes.has(name) ? name : null]));
  const banner = '// 由 tools/mock/generate.mjs 根据 web/src/api/openapi.json 与 core/api/generated/wire.ts 生成，禁止手改。\n';
  const body = `export const mockOperations = ${JSON.stringify(stable(routes), null, 2)} as const;\n\nexport const schemaWireTypes = ${JSON.stringify(schemaWireTypes, null, 2)} as const;\n`;
  return [{ path: 'operations.ts', content: `${banner}${body}` }];
}

export function validateNoManualPathDispatch(files: Readonly<Record<string, string>>): Violation[] {
  const violations: Violation[] = [];
  for (const [path, source] of Object.entries(files)) {
    const segments = path.replaceAll('\\', '/').split('/');
    if (segments[0] === 'mock' && (segments[1] === 'generated' || segments[1] === 'scenarios')) continue;
    for (const match of source.matchAll(/(['"`])(\/(?:api\/)?[^'"`{}\s]*\{[^'"`{}\s]+\}[^'"`\s]*)\1/g)) {
      violations.push({ rule: 'no-manual-path-dispatch', location: path, message: `path template ${match[2]} must come from mock/generated` });
    }
  }
  return violations;
}
