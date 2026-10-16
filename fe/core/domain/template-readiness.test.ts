import { expect, it } from 'vitest';
import { trackTemplateSchema } from './track.js';

it('requires explicit author metadata and server availability, including a complete missing prerequisite', () => {
  const legacy = { id: 'site/custom', title: 'Custom work', tasks: [] };
  expect(trackTemplateSchema.safeParse(legacy).success).toBe(false);
  expect(trackTemplateSchema.safeParse({ ...legacy, description: null }).success).toBe(false);
  expect(trackTemplateSchema.safeParse({ ...legacy, availability: { status: 'ready' } }).success).toBe(false);
  const purpose = { ...legacy, description: 'Produce a report.' };
  expect(trackTemplateSchema.parse({ ...purpose, availability: { status: 'ready' } }).description).toBe('Produce a report.');
  expect(trackTemplateSchema.safeParse({ ...purpose, availability: { status: 'requires_plugin', plugin_id: 'tools' } }).success).toBe(false);
  expect(trackTemplateSchema.parse({ ...purpose, availability: { status: 'requires_plugin', plugin_id: 'tools', plugin_name: 'Tools' } }).availability.status).toBe('requires_plugin');
});
