import { z } from 'zod';
import { parseGitHubIssueUrl } from './issue-url.js';

const textFieldSchema = z.object({
  kind: z.literal('text'), key: z.string().min(1), label: z.string().min(1), default: z.string(),
  required: z.boolean(), placeholder: z.string(), help: z.string(),
  format: z.object({ kind: z.literal('github-issue-url'), outputs: z.record(z.string(), z.string().min(1)) }).strict().nullable(),
}).strict();
const toggleFieldSchema = z.object({
  kind: z.literal('toggle'), key: z.string().min(1), label: z.string().min(1), default: z.string(),
  on_value: z.string(), off_value: z.string(), on_description: z.string(), off_description: z.string(),
}).strict();
export const templateInputFormSchema = z.object({
  version: z.literal(1), groups: z.array(z.object({
    title: z.string().min(1), description: z.string(),
    fields: z.array(z.discriminatedUnion('kind', [textFieldSchema, toggleFieldSchema])).min(1),
  }).strict()).min(1),
}).strict().superRefine((form, ctx) => {
  const keys = new Set<string>();
  for (const group of form.groups) for (const field of group.fields) {
    if (keys.has(field.key)) ctx.addIssue({ code: 'custom', message: 'Duplicate field key' });
    keys.add(field.key);
    if (field.kind === 'toggle' && (field.on_value === field.off_value || ![field.on_value, field.off_value].includes(field.default))) {
      ctx.addIssue({ code: 'custom', message: 'Invalid toggle values or default' });
    }
  }
});
export type TemplateInputForm = z.infer<typeof templateInputFormSchema>;
export type TemplateInputField = TemplateInputForm['groups'][number]['fields'][number];
export type TemplateInputValues = Readonly<Record<string, string>>;

export function templateFieldValue(field: TemplateInputField, values: TemplateInputValues): string {
  return Object.hasOwn(values, field.key) ? values[field.key] : field.default;
}

/** One explicit declaration; malformed or duplicate declarations never become an empty form. */
export function readTemplateInputForm(body: string): TemplateInputForm | null {
  const matches = [...body.matchAll(/<!--\s*neige:input-form\s+([\s\S]*?)-->/g)];
  if (matches.length !== 1) return null;
  try {
    const result = templateInputFormSchema.safeParse(JSON.parse(matches[0][1]));
    return result.success ? result.data : null;
  } catch { return null; }
}

const inputSchema = z.object({
  type: z.literal('object'),
  properties: z.record(z.string(), z.object({ type: z.enum(['string', 'integer', 'number', 'boolean']), enum: z.array(z.unknown()).optional() }).passthrough()),
  required: z.array(z.string()).optional(),
}).passthrough();

type CompiledInputs =
  | Readonly<{ status: 'unsupported'; form: null }>
  | Readonly<{ status: 'ready' | 'incomplete'; form: TemplateInputForm; input: Readonly<Record<string, unknown>>; errors: Readonly<Record<string, string>> }>;

/** Fixed converters produce data only. Form declarations never run code or grant tools. */
export function compileTemplateInputs(body: string, schemaValue: unknown, values: TemplateInputValues): CompiledInputs {
  const form = readTemplateInputForm(body);
  const schema = inputSchema.safeParse(schemaValue);
  if (form === null || !schema.success) return { status: 'unsupported', form: null };
  const owners = new Map<string, Readonly<{ source: string; output: string }>>();
  const declare = (target: string, source: string, type: string, output: string) => {
    const property = schema.data.properties[target];
    if (!Object.hasOwn(schema.data.properties, target)) return false;
    if (property.type !== type && !(type === 'integer' && property.type === 'number')) return false;
    const prior = owners.get(target);
    if (prior !== undefined && (prior.source !== source || prior.output !== output)) return false;
    owners.set(target, { source, output });
    return true;
  };
  const input = Object.create(null) as Record<string, unknown>;
  const errors = Object.create(null) as Record<string, string>;
  for (const group of form.groups) for (const field of group.fields) {
    if (!declare(field.key, field.key, 'string', field.kind === 'text' && field.format !== null ? 'issue_url' : 'value')) return { status: 'unsupported', form: null };
    const value = templateFieldValue(field, values);
    input[field.key] = value;
    if (field.kind === 'toggle') {
      if (![field.on_value, field.off_value].includes(value)) errors[field.key] = 'Choose a declared option.';
      const allowed = schema.data.properties[field.key].enum;
      if (allowed !== undefined && ![field.on_value, field.off_value].every((choice) => allowed.includes(choice))) return { status: 'unsupported', form: null };
    } else {
      if (field.required && value.trim() === '') errors[field.key] = `${field.label} is required.`;
      if (field.format !== null) {
        for (const [output, target] of Object.entries(field.format.outputs)) {
          if (!['issue_url', 'repo', 'issue_number'].includes(output) || !declare(target, field.key, output === 'issue_number' ? 'integer' : 'string', output)) return { status: 'unsupported', form: null };
        }
        if (!field.required && value.trim() === '') {
          delete input[field.key];
          continue;
        }
        const parsed = parseGitHubIssueUrl(value);
        if (parsed === null) errors[field.key] = 'Not a GitHub issue URL — expected https://github.com/owner/repo/issues/123.';
        else {
          input[field.key] = parsed.issue_url;
          for (const [output, target] of Object.entries(field.format.outputs)) input[target] = parsed[output as keyof typeof parsed];
        }
      }
    }
  }
  for (const required of schema.data.required ?? []) {
    if (!owners.has(required)) return { status: 'unsupported', form: null };
    if (!Object.hasOwn(input, required)) errors[owners.get(required)!.source] ??= 'Complete this field.';
  }
  for (const [key, value] of Object.entries(input)) {
    const property = schema.data.properties[key];
    if (property.enum !== undefined && !property.enum.includes(value)) errors[owners.get(key)!.source] = 'Choose a value allowed by this template.';
  }
  return { status: Object.keys(errors).length === 0 ? 'ready' : 'incomplete', form, input, errors };
}
