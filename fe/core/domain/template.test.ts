import { expect, it } from 'vitest';
import { templateDetailOperation, templateDetailSchema } from './template.js';

it('decodes author metadata and requires the actual source and identity', () => {
  const detail = { id: 'site/custom', title: 'Custom', description: null, instructions: null, body: '# Source' };
  expect(templateDetailSchema.parse(detail)).toEqual(detail);
  expect(templateDetailSchema.safeParse({ ...detail, body: undefined }).success).toBe(false);
  expect(templateDetailSchema.safeParse({ ...detail, id: undefined }).success).toBe(false);
  expect(templateDetailSchema.safeParse({ ...detail, instructions: undefined }).success).toBe(false);
  expect(templateDetailOperation('site/custom').path).toBe('/api/track-templates/site%2Fcustom');
});
