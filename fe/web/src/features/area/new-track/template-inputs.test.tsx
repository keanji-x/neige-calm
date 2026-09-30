import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { NewTrackForm } from './public.tsx';

afterEach(cleanup);

it('collects a template-declared field for a template with an unrelated id', async () => {
  const form = { version: 1, groups: [{ title: 'Project', description: 'Choose the project to create.', fields: [{
    kind: 'text', key: 'project_code', label: 'Project code', default: '', required: true,
    placeholder: 'Project identifier', help: 'Required for this template.', format: null,
  }] }] };
  const onSubmit = vi.fn();
  render(<NewTrackForm submitting={false} error={null}
    templates={[{ id: 'custom-create', title: 'Custom create', tasks: [], input_schema: {
      type: 'object', properties: { project_code: { type: 'string' } }, required: ['project_code'], additionalProperties: false,
    } }]} templatesLoaded initialTemplateId="custom-create" initialCwd={null}
    loadTemplate={(id) => Promise.resolve({ id, title: 'Custom create', description: null, instructions: null,
      body: `<!-- neige:input-form ${JSON.stringify(form)} -->` })}
    onManageRecipes={vi.fn()} listDirectory={vi.fn()} onSubmit={onSubmit} />);
  await userEvent.type(await screen.findByLabelText('Project code'), 'calm');
  await userEvent.click(screen.getByLabelText('What this track should do'));
  await userEvent.type(screen.getByLabelText('What this track should do'), 'Create the project');
  await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
  expect(onSubmit).toHaveBeenCalledWith({ message: 'Create the project', template_id: 'custom-create', template_input: { project_code: 'calm' } });
});
