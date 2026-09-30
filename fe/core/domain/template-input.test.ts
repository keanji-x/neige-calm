import { expect, it } from 'vitest';
import { compileTemplateInputs, readTemplateInputForm } from './template-input.js';

function contract() {
  const form = { version: 1, groups: [{ title: 'Source', description: '', fields: [{
    kind: 'text', key: 'ticket', label: 'Ticket URL', default: '', required: true, placeholder: '', help: '',
    format: { kind: 'github-issue-url', outputs: { repo: 'project', issue_number: 'number' } },
  }] }, { title: 'Delivery', description: '', fields: [{ kind: 'toggle', key: 'action', label: 'Publish',
    default: 'hold', on_value: 'publish', off_value: 'hold', on_description: 'Publish after checks.', off_description: 'Wait for approval.',
  }] }] };
  const schema = { type: 'object', properties: { ticket: { type: 'string' }, project: { type: 'string' },
    number: { type: 'integer' }, action: { type: 'string', enum: ['hold', 'publish'] } }, required: ['ticket', 'project', 'number', 'action'] };
  const body = `<!-- neige:input-form ${JSON.stringify(form)} -->`;
  return { form, schema, body };
}

it('uses declared aliases, groups and defaults without issue-development field names', () => {
  const { body, schema } = contract();
  const result = compileTemplateInputs(body, schema, { ticket: 'https://github.com/owner/repo/issues/12' });
  expect(result.status).toBe('ready');
  if (result.status === 'unsupported') throw new Error('Unexpected unsupported contract');
  expect(result.input).toEqual({ ticket: 'https://github.com/owner/repo/issues/12', project: 'owner/repo', number: 12, action: 'hold' });
  expect(result.form.groups.map((group) => group.title)).toEqual(['Source', 'Delivery']);
});

it('retains the specific URL failure when derived required fields are missing', () => {
  const { body, schema } = contract();
  const result = compileTemplateInputs(body, schema, { ticket: 'not a url' });
  expect(result.status).toBe('incomplete');
  if (result.status === 'unsupported') throw new Error('Unexpected unsupported contract');
  expect(result.errors.ticket).toContain('Not a GitHub issue URL');
  expect(Object.hasOwn(result.input, 'number')).toBe(false);
});

it('rejects required plugin fields with no declared source', () => {
  const { body, schema } = contract();
  schema.required.push('missing');
  expect(compileTemplateInputs(body, schema, { ticket: 'https://github.com/owner/repo/issues/12' }).status).toBe('unsupported');
});

it('rejects malformed, duplicate and conflicting declarations', () => {
  const { body, form, schema } = contract();
  expect(readTemplateInputForm('<!-- neige:input-form {invalid} -->')).toBeNull();
  expect(readTemplateInputForm(body + body)).toBeNull();
  form.groups.push(form.groups[0]);
  expect(readTemplateInputForm(`<!-- neige:input-form ${JSON.stringify(form)} -->`)).toBeNull();
  expect(compileTemplateInputs(body, { ...schema, properties: { ...schema.properties, number: { type: 'string' } } }, {}).status).toBe('unsupported');
});

it('refuses toggle values outside the bound plugin enum', () => {
  const { body, schema } = contract();
  expect(compileTemplateInputs(body, { ...schema, properties: { ...schema.properties, action: { type: 'string', enum: ['hold'] } } }, {}).status).toBe('unsupported');
});


it('allows an integer converter output in a number-valued plugin field', () => {
  const { body, schema } = contract();
  const numericSchema = { ...schema, properties: { ...schema.properties, number: { type: 'number' } } };
  expect(compileTemplateInputs(body, numericSchema, { ticket: 'https://github.com/owner/repo/issues/12' }).status).toBe('ready');
});
