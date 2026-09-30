import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const check = process.argv.includes('--check');
const base = new URL('../../core/domain/', import.meta.url);
/** @type {NodeJS.ProcessEnv} */
const env = { ...process.env, RUSTC_WRAPPER: '', CARGO_BUILD_JOBS: '6' };
delete env.NEIGE_CODEX_BIN;
/** @param {string} mode */
const emit = mode => execFileSync('cargo', ['run', '--quiet', '--locked', '-p', 'calm-types',
  '--example', 'export_report_view_contract', '--', mode], { cwd: root, env, encoding: 'utf8' });
const schemaText = emit('--schema');
const schema = JSON.parse(schemaText);
const definitions = schema.$defs;
if (!definitions) throw new Error('Expected crate-owned presentation definitions');
const emitted = new Set();
const visiting = new Set();
const lines = ["// Generated from calm-types presentation DTOs; run node tools/report-view/generate.mjs.",
  "import { z } from 'zod';", "import { isCalendarDate } from './report-date.js';", ''];
/** @param {string} name */
const binding = name => `shape${name.replace(/[^A-Za-z0-9_]/g, '_')}`;

// A deliberately narrow compiler for our generated structural schema. Unknown
// validation keywords fail generation; refinements are not silently discarded.
/** @param {any} node @returns {string} */
function expression(node) {
  if (node === true) return 'z.unknown()';
  if (node === false) return 'z.never()';
  const supported = new Set(['$schema', '$ref', '$defs', 'title', 'description', 'deprecated',
    'example', 'examples', 'default', 'discriminator', 'type', 'enum', 'const', 'oneOf', 'anyOf', 'allOf',
    'properties', 'required', 'additionalProperties', 'items', 'minItems', 'maxItems',
    'minLength', 'maxLength', 'minimum', 'maximum', 'exclusiveMinimum', 'exclusiveMaximum', 'multipleOf', 'pattern', 'format', 'propertyNames']);
  for (const key of Object.keys(node)) if (!supported.has(key)) throw new Error(`Unsupported schema keyword: ${key}`);
  if (node.$ref) {
    if (!node.$ref.startsWith('#/$defs/')) throw new Error(`Nonlocal schema reference ${node.$ref}`);
    const name = node.$ref.slice(8);
    definition(name);
    return binding(name);
  }
  if (node.const !== undefined) return `z.literal(${JSON.stringify(node.const)})`;
  if (node.enum?.length === 1) return `z.literal(${JSON.stringify(node.enum[0])})`;
  if (node.enum) return `z.union([${node.enum.map((/** @type {unknown} */ value) => `z.literal(${JSON.stringify(value)})`).join(', ')}])`;
  if (node.oneOf || node.anyOf) {
    const items = node.oneOf ?? node.anyOf;
    const values = items.map(expression);
    return values.length === 1 ? values[0] : `z.union([${values.join(', ')}])`;
  }
  if (node.allOf) return node.allOf.map(expression).reduce((/** @type {string} */ a, /** @type {string} */ b) => `z.intersection(${a}, ${b})`);
  if (Array.isArray(node.type)) return `z.union([${node.type.map((/** @type {string} */ type) => expression({ ...node, type })).join(', ')}])`;
  switch (node.type) {
    case 'null': return 'z.null()';
    case 'boolean': return 'z.boolean()';
    case 'integer':
    case 'number': {
      let result = node.type === 'integer' ? 'z.number().int()' : 'z.number()';
      if (node.minimum !== undefined) result += `.min(${node.minimum})`;
      if (node.maximum !== undefined) result += `.max(${node.maximum})`;
      if (node.exclusiveMinimum !== undefined) result += `.gt(${node.exclusiveMinimum})`;
      if (node.exclusiveMaximum !== undefined) result += `.lt(${node.exclusiveMaximum})`;
      if (node.multipleOf !== undefined) result += `.multipleOf(${node.multipleOf})`;
      if (node.format && !['float', 'double', 'int32', 'int64', 'uint32', 'uint64'].includes(node.format)) throw new Error(`Unsupported numeric format ${node.format}`);
      return result;
    }
    case 'string': {
      let result = 'z.string()';
      if (node.minLength !== undefined) result += `.refine(value => [...value].length >= ${node.minLength}, 'Text below protocol minimum')`;
      if (node.maxLength !== undefined) result += `.refine(value => [...value].length <= ${node.maxLength}, 'Text exceeds protocol limit')`;
      if (node.pattern) result += `.regex(new RegExp(${JSON.stringify(node.pattern)}))`;
      if (node.format === 'date') result += ".refine(value => !value.startsWith('0000') && isCalendarDate(value), 'Expected UTC calendar date')";
      else if (node.format) throw new Error(`Unsupported string format ${node.format}`);
      return result;
    }
    case 'array': {
      let result = `z.array(${expression(node.items)})`;
      if (node.minItems !== undefined) result += `.min(${node.minItems})`;
      if (node.maxItems !== undefined) result += `.max(${node.maxItems})`;
      return result;
    }
    case 'object': {
      if (!node.properties) {
        if (!node.additionalProperties || node.additionalProperties === true) throw new Error('Unbounded presentation object');
        return `z.record(${node.propertyNames ? expression(node.propertyNames) : 'z.string()'}, ${expression(node.additionalProperties)})`;
      }
      if (node.additionalProperties !== false) throw new Error('Presentation structs must be closed');
      const required = new Set(node.required ?? []);
      return `z.strictObject({${Object.entries(node.properties).map(([key, field]) =>
        `${JSON.stringify(key)}: ${expression(field)}${required.has(key) ? '' : '.optional()'}`).join(', ')}})`;
    }
    default: throw new Error(`Missing/unsupported presentation type: ${JSON.stringify(node)}`);
  }
}
/** @param {string} name */
function definition(name) {
  if (emitted.has(name)) return;
  if (visiting.has(name)) throw new Error(`Recursive presentation contract ${name}`);
  if (!definitions[name]) throw new Error(`Missing presentation definition ${name}`);
  visiting.add(name);
  const value = expression(definitions[name]);
  visiting.delete(name);
  emitted.add(name);
  lines.push(`const ${binding(name)} = ${value};`);
}
definition('NativeView');
lines.push('', `export const nativeViewShapeSchema = ${binding('NativeView')};`, '');
/** @type {Array<[URL, string]>} */
const outputs = [
  [new URL('../../../crates/calm-types/src/report_blocks/native_view.schema.json', import.meta.url), schemaText],
  [new URL('report-view.generated.ts', base), lines.join('\n')],
  [new URL('report-view.types.generated.ts', base), '// Generated from calm-types; do not edit.\n' + emit('--types')],
];
for (const [url, text] of outputs) {
  if (check) {
    if (readFileSync(url, 'utf8') !== text) throw new Error(`Generated presentation drift: ${fileURLToPath(url)}`);
  } else writeFileSync(url, text);
}
